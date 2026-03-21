/*
 * µScope DPI Bridge — C writer library implementation.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

#include "uscope_dpi.h"
#include "lz4.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <assert.h>

/* ── Wire-format constants ──────────────────────────────────────── */

static const uint8_t MAGIC[4] = {0x75, 0x53, 0x43, 0x50}; /* "uSCP" */
static const uint8_t SEG_MAGIC[4] = {0x75, 0x53, 0x45, 0x47}; /* "uSEG" */

#define F_COMPLETE       (1ULL << 0)
#define F_COMPRESSED     (1ULL << 1)
#define F_HAS_STRINGS    (1ULL << 2)
#define F_COMPACT_DELTAS (1ULL << 6)
#define F_INTERLEAVED_DELTAS (1ULL << 7)

#define TAG_WIDE_OP    0x01
#define TAG_COMPACT_OP 0x02
#define TAG_EVENT      0x03

#define CHUNK_END         0x0000
#define CHUNK_DUT_DESC    0x0001
#define CHUNK_SCHEMA      0x0002
#define CHUNK_TRACE_CONFIG 0x0003

#define SECTION_END       0x0000
#define SECTION_SUMMARY   0x0001
#define SECTION_STRINGS   0x0002
#define SECTION_SEGMENTS  0x0003

#define DA_SLOT_SET       0x01
#define DA_SLOT_CLEAR     0x02
#define DA_SLOT_ADD       0x03

#define SF_SPARSE         0x0001

/* File header: 48 bytes */
#define FILE_HEADER_SIZE  48
/* Segment header: 56 bytes */
#define SEG_HEADER_SIZE   56

/* ── Helper: little-endian write ────────────────────────────────── */

static void write_u8(FILE *f, uint8_t v) { fwrite(&v, 1, 1, f); }
static void write_u16(FILE *f, uint16_t v) { uint8_t b[2]; b[0]=v; b[1]=v>>8; fwrite(b,1,2,f); }
static void write_u32(FILE *f, uint32_t v) { uint8_t b[4]; b[0]=v; b[1]=v>>8; b[2]=v>>16; b[3]=v>>24; fwrite(b,1,4,f); }
static void write_u64(FILE *f, uint64_t v) {
    uint8_t b[8];
    for (int i=0;i<8;i++) b[i]=(uint8_t)(v>>(i*8));
    fwrite(b,1,8,f);
}

static void buf_u8(uint8_t **p, uint8_t v) { *(*p)++ = v; }
static void buf_u16(uint8_t **p, uint16_t v) { *(*p)++ = (uint8_t)v; *(*p)++ = (uint8_t)(v>>8); }
static void buf_u32(uint8_t **p, uint32_t v) {
    *(*p)++ = (uint8_t)v; *(*p)++ = (uint8_t)(v>>8);
    *(*p)++ = (uint8_t)(v>>16); *(*p)++ = (uint8_t)(v>>24);
}
static void buf_u64(uint8_t **p, uint64_t v) {
    for (int i=0;i<8;i++) *(*p)++ = (uint8_t)(v>>(i*8));
}

/* LEB128 encode into buffer, returns bytes written */
static int leb128_encode(uint64_t value, uint8_t *buf) {
    int n = 0;
    do {
        uint8_t byte = value & 0x7F;
        value >>= 7;
        if (value != 0) byte |= 0x80;
        buf[n++] = byte;
    } while (value != 0);
    return n;
}

/* ── String pool (schema-level, max 64 KB) ──────────────────────── */

#define MAX_STRING_POOL (64*1024)

typedef struct {
    uint8_t  data[MAX_STRING_POOL];
    uint16_t len;
} string_pool_t;

static void sp_init(string_pool_t *sp) { sp->len = 0; }

static uint16_t sp_insert(string_pool_t *sp, const char *s) {
    /* Linear scan for dedup (acceptable for schema-time strings) */
    uint16_t off = 0;
    while (off < sp->len) {
        if (strcmp((char*)sp->data + off, s) == 0) return off;
        off += (uint16_t)(strlen((char*)sp->data + off) + 1);
    }
    uint16_t slen = (uint16_t)strlen(s);
    assert(sp->len + slen + 1 <= MAX_STRING_POOL);
    uint16_t result = sp->len;
    memcpy(sp->data + sp->len, s, slen + 1);
    sp->len += slen + 1;
    return result;
}

/* ── Schema definition ──────────────────────────────────────────── */

#define MAX_CLOCKS    16
#define MAX_SCOPES    256
#define MAX_ENUMS     64
#define MAX_ENUM_VALS 256
#define MAX_STORAGES  256
#define MAX_EVENTS    256
#define MAX_FIELDS    32

typedef struct {
    uint16_t name;   /* string pool offset */
    uint8_t  type;
    uint8_t  enum_id;
} field_def_t;

typedef struct {
    uint16_t name;
    uint16_t clock_id;
    uint32_t period_ps;
} clock_def_t;

typedef struct {
    uint16_t name;
    uint16_t scope_id;
    uint16_t parent_id;
    uint16_t protocol;
    uint8_t  clock_id;
} scope_def_t;

typedef struct {
    uint8_t  value;
    uint16_t name;
} enum_val_t;

typedef struct {
    uint16_t    name;
    uint8_t     num_values;
    enum_val_t  values[MAX_ENUM_VALS];
} enum_def_t;

typedef struct {
    uint16_t   name;
    uint16_t   storage_id;
    uint16_t   num_slots;
    uint16_t   num_fields;
    uint16_t   flags;
    uint16_t   scope_id;
    field_def_t fields[MAX_FIELDS];
} storage_def_t;

typedef struct {
    uint16_t   name;
    uint16_t   event_type_id;
    uint16_t   num_fields;
    uint16_t   scope_id;
    field_def_t fields[MAX_FIELDS];
} event_def_t;

struct uscope_schema {
    string_pool_t sp;
    clock_def_t   clocks[MAX_CLOCKS];
    uint8_t       num_clocks;
    scope_def_t   scopes[MAX_SCOPES];
    uint16_t      num_scopes;
    enum_def_t    enums[MAX_ENUMS];
    uint8_t       num_enums;
    storage_def_t storages[MAX_STORAGES];
    uint16_t      num_storages;
    event_def_t   events[MAX_EVENTS];
    uint16_t      num_events;
};

/* ── Schema API implementation ──────────────────────────────────── */

uscope_schema_def_t *uscope_schema_new(void) {
    uscope_schema_def_t *s = calloc(1, sizeof(*s));
    sp_init(&s->sp);
    return s;
}

void uscope_schema_free(uscope_schema_def_t *s) { free(s); }

uint8_t uscope_schema_add_clock(uscope_schema_def_t *s,
                                const char *name,
                                uint32_t period_ps) {
    uint8_t id = s->num_clocks++;
    s->clocks[id].name = sp_insert(&s->sp, name);
    s->clocks[id].clock_id = id;
    s->clocks[id].period_ps = period_ps;
    return id;
}

uint16_t uscope_schema_add_scope(uscope_schema_def_t *s,
                                 const char *name,
                                 uint16_t parent,
                                 const char *protocol,
                                 uint8_t clock_id) {
    uint16_t id = s->num_scopes++;
    s->scopes[id].name = sp_insert(&s->sp, name);
    s->scopes[id].scope_id = id;
    s->scopes[id].parent_id = parent;
    s->scopes[id].protocol = protocol ? sp_insert(&s->sp, protocol) : 0xFFFF;
    s->scopes[id].clock_id = clock_id;
    return id;
}

uint8_t uscope_schema_add_enum(uscope_schema_def_t *s,
                               const char *name,
                               const char **values,
                               uint8_t count) {
    uint8_t id = s->num_enums++;
    s->enums[id].name = sp_insert(&s->sp, name);
    s->enums[id].num_values = count;
    for (uint8_t i = 0; i < count; i++) {
        s->enums[id].values[i].value = i;
        s->enums[id].values[i].name = sp_insert(&s->sp, values[i]);
    }
    return id;
}

uint16_t uscope_schema_add_storage(uscope_schema_def_t *s,
                                   const char *name,
                                   uint16_t scope_id,
                                   uint16_t num_slots,
                                   uint16_t flags,
                                   uint16_t num_fields,
                                   const char **field_names,
                                   const uint8_t *field_types,
                                   const uint8_t *field_enum_ids) {
    uint16_t id = s->num_storages++;
    storage_def_t *st = &s->storages[id];
    st->name = sp_insert(&s->sp, name);
    st->storage_id = id;
    st->num_slots = num_slots;
    st->num_fields = num_fields;
    st->flags = flags;
    st->scope_id = scope_id;
    for (uint16_t i = 0; i < num_fields; i++) {
        st->fields[i].name = sp_insert(&s->sp, field_names[i]);
        st->fields[i].type = field_types[i];
        st->fields[i].enum_id = field_enum_ids ? field_enum_ids[i] : 0;
    }
    return id;
}

uint16_t uscope_schema_add_event(uscope_schema_def_t *s,
                                 const char *name,
                                 uint16_t scope_id,
                                 uint16_t num_fields,
                                 const char **field_names,
                                 const uint8_t *field_types,
                                 const uint8_t *field_enum_ids) {
    uint16_t id = s->num_events++;
    event_def_t *ev = &s->events[id];
    ev->name = sp_insert(&s->sp, name);
    ev->event_type_id = id;
    ev->num_fields = num_fields;
    ev->scope_id = scope_id;
    for (uint16_t i = 0; i < num_fields; i++) {
        ev->fields[i].name = sp_insert(&s->sp, field_names[i]);
        ev->fields[i].type = field_types[i];
        ev->fields[i].enum_id = field_enum_ids ? field_enum_ids[i] : 0;
    }
    return id;
}

/* ── Schema serialization ───────────────────────────────────────── */

static int field_type_size(uint8_t ft) {
    switch (ft) {
        case USCOPE_FT_U8: case USCOPE_FT_I8: case USCOPE_FT_BOOL: case USCOPE_FT_ENUM: return 1;
        case USCOPE_FT_U16: case USCOPE_FT_I16: return 2;
        case USCOPE_FT_U32: case USCOPE_FT_I32: case USCOPE_FT_STRING_REF: return 4;
        case USCOPE_FT_U64: case USCOPE_FT_I64: return 8;
        default: return 0;
    }
}

static int storage_slot_size(const storage_def_t *st) {
    int sz = 0;
    for (uint16_t i = 0; i < st->num_fields; i++)
        sz += field_type_size(st->fields[i].type);
    return sz;
}

static void write_schema(FILE *f, const uscope_schema_def_t *s) {
    /* Compute string pool offset */
    uint16_t sp_offset = 14; /* header */
    sp_offset += s->num_clocks * 8;
    sp_offset += s->num_scopes * 12;
    for (uint8_t i = 0; i < s->num_enums; i++)
        sp_offset += 4 + s->enums[i].num_values * 4;
    for (uint16_t i = 0; i < s->num_storages; i++)
        sp_offset += 12 + s->storages[i].num_fields * 8;
    for (uint16_t i = 0; i < s->num_events; i++)
        sp_offset += 8 + s->events[i].num_fields * 8;
    /* No summary fields for now */

    /* Schema header (14 bytes) */
    write_u8(f, s->num_enums);
    write_u8(f, s->num_clocks);
    write_u16(f, s->num_scopes);
    write_u16(f, s->num_storages);
    write_u16(f, s->num_events);
    write_u16(f, 0); /* num_summary_fields */
    write_u16(f, sp_offset);

    /* Clock domains */
    for (uint8_t i = 0; i < s->num_clocks; i++) {
        write_u16(f, s->clocks[i].name);
        write_u16(f, s->clocks[i].clock_id);
        write_u32(f, s->clocks[i].period_ps);
    }

    /* Scopes */
    for (uint16_t i = 0; i < s->num_scopes; i++) {
        write_u16(f, s->scopes[i].name);
        write_u16(f, s->scopes[i].scope_id);
        write_u16(f, s->scopes[i].parent_id);
        write_u16(f, s->scopes[i].protocol);
        write_u8(f, s->scopes[i].clock_id);
        uint8_t reserved[3] = {0,0,0};
        fwrite(reserved, 1, 3, f);
    }

    /* Enums */
    for (uint8_t i = 0; i < s->num_enums; i++) {
        write_u16(f, s->enums[i].name);
        write_u8(f, s->enums[i].num_values);
        write_u8(f, 0); /* reserved */
        for (uint8_t j = 0; j < s->enums[i].num_values; j++) {
            write_u8(f, s->enums[i].values[j].value);
            write_u8(f, 0); /* reserved */
            write_u16(f, s->enums[i].values[j].name);
        }
    }

    /* Storages */
    for (uint16_t i = 0; i < s->num_storages; i++) {
        const storage_def_t *st = &s->storages[i];
        write_u16(f, st->name);
        write_u16(f, st->storage_id);
        write_u16(f, st->num_slots);
        write_u16(f, st->num_fields);
        write_u16(f, st->flags);
        write_u16(f, st->scope_id);
        for (uint16_t j = 0; j < st->num_fields; j++) {
            write_u16(f, st->fields[j].name);
            write_u8(f, st->fields[j].type);
            write_u8(f, st->fields[j].enum_id);
            uint8_t reserved[4] = {0,0,0,0};
            fwrite(reserved, 1, 4, f);
        }
    }

    /* Events */
    for (uint16_t i = 0; i < s->num_events; i++) {
        const event_def_t *ev = &s->events[i];
        write_u16(f, ev->name);
        write_u16(f, ev->event_type_id);
        write_u16(f, ev->num_fields);
        write_u16(f, ev->scope_id);
        for (uint16_t j = 0; j < ev->num_fields; j++) {
            write_u16(f, ev->fields[j].name);
            write_u8(f, ev->fields[j].type);
            write_u8(f, ev->fields[j].enum_id);
            uint8_t reserved[4] = {0,0,0,0};
            fwrite(reserved, 1, 4, f);
        }
    }

    /* String pool */
    fwrite(s->sp.data, 1, s->sp.len, f);
}

/* ── Internal writer state ──────────────────────────────────────── */

#define MAX_OPS_PER_FRAME   4096
#define MAX_EVENTS_PER_FRAME 1024
#define MAX_SEGMENTS        65536
#define MAX_STRINGS         65536
#define MAX_DELTA_BUF       (4*1024*1024) /* 4 MB delta buffer */

typedef struct {
    uint8_t  action;
    uint16_t storage_id;
    uint16_t slot_index;
    uint16_t field_index;
    uint64_t value;
} delta_op_t;

typedef struct {
    uint16_t event_type_id;
    uint32_t payload_size;
    uint8_t  payload[256]; /* max event payload */
} event_rec_t;

typedef struct {
    uint64_t offset;
    uint64_t time_start_ps;
    uint64_t time_end_ps;
} seg_index_entry_t;

/* Per-storage state for checkpointing */
typedef struct {
    uint16_t storage_id;
    uint16_t num_slots;
    uint16_t num_fields;
    uint16_t flags;
    int      slot_size;
    uint8_t  *valid;    /* [num_slots] */
    uint8_t  *data;     /* [num_slots * slot_size] */
    int      *field_offsets; /* [num_fields] */
    uint8_t  *field_types;   /* [num_fields] */
} storage_state_t;

/* String table entry */
typedef struct {
    char    *str;
    uint32_t len;
} string_entry_t;

struct uscope_writer {
    FILE *fp;
    uint64_t flags;
    uint64_t total_time_ps;
    uint32_t num_segments;
    uint32_t preamble_end;
    uint64_t section_table_offset;
    uint64_t tail_offset;
    uint64_t checkpoint_interval_ps;

    /* Schema info */
    uint16_t num_storages;
    storage_state_t *states;

    /* Current frame */
    delta_op_t  ops[MAX_OPS_PER_FRAME];
    uint16_t    num_ops;
    event_rec_t events[MAX_EVENTS_PER_FRAME];
    uint16_t    num_events;
    /* v0.2 interleaved order tracking: (is_event<<15) | index */
    uint16_t    item_order[MAX_OPS_PER_FRAME + MAX_EVENTS_PER_FRAME];
    uint16_t    num_items;
    uint64_t    current_time_ps;
    int         in_cycle;

    /* Delta buffer */
    uint8_t  *delta_buf;
    uint32_t  delta_buf_len;
    uint32_t  delta_buf_cap;
    uint64_t  seg_time_start;
    uint64_t  seg_time_end;
    uint32_t  seg_num_frames;
    uint32_t  seg_num_frames_active;
    uint64_t  last_frame_time_ps;

    /* Segment index */
    seg_index_entry_t *seg_index;
    uint32_t seg_index_len;
    uint64_t prev_seg_offset;
    uint64_t next_checkpoint_ps;

    /* String table */
    string_entry_t *strings;
    uint32_t num_strings;
    uint32_t strings_cap;
};

/* ── Storage state helpers ──────────────────────────────────────── */

static void state_init(storage_state_t *st, const storage_def_t *def) {
    st->storage_id = def->storage_id;
    st->num_slots = def->num_slots;
    st->num_fields = def->num_fields;
    st->flags = def->flags;

    st->field_offsets = calloc(def->num_fields, sizeof(int));
    st->field_types = calloc(def->num_fields, sizeof(uint8_t));
    int off = 0;
    for (uint16_t i = 0; i < def->num_fields; i++) {
        st->field_offsets[i] = off;
        st->field_types[i] = def->fields[i].type;
        off += field_type_size(def->fields[i].type);
    }
    st->slot_size = off;

    st->valid = calloc(def->num_slots, 1);
    st->data = calloc(def->num_slots, off > 0 ? off : 1);
}

static void state_free(storage_state_t *st) {
    free(st->valid);
    free(st->data);
    free(st->field_offsets);
    free(st->field_types);
}

static void state_set_field(storage_state_t *st, uint16_t slot,
                            uint16_t field, uint64_t value) {
    if (slot >= st->num_slots || field >= st->num_fields) return;
    st->valid[slot] = 1;
    uint8_t *p = st->data + (size_t)slot * st->slot_size + st->field_offsets[field];
    int sz = field_type_size(st->field_types[field]);
    /* Store little-endian */
    for (int i = 0; i < sz; i++)
        p[i] = (uint8_t)(value >> (i*8));
}

static uint64_t state_get_field(const storage_state_t *st, uint16_t slot,
                                uint16_t field) {
    if (slot >= st->num_slots || field >= st->num_fields) return 0;
    const uint8_t *p = st->data + (size_t)slot * st->slot_size + st->field_offsets[field];
    int sz = field_type_size(st->field_types[field]);
    uint64_t v = 0;
    for (int i = 0; i < sz; i++)
        v |= (uint64_t)p[i] << (i*8);
    return v;
}

static void state_add_field(storage_state_t *st, uint16_t slot,
                            uint16_t field, uint64_t value) {
    uint64_t cur = state_get_field(st, slot, field);
    state_set_field(st, slot, field, cur + value);
}

static void state_clear_slot(storage_state_t *st, uint16_t slot) {
    if (slot >= st->num_slots) return;
    st->valid[slot] = 0;
    memset(st->data + (size_t)slot * st->slot_size, 0, st->slot_size);
}

/* ── Checkpoint serialization ───────────────────────────────────── */

static uint32_t write_checkpoint_to_buf(uint8_t *buf, const storage_state_t *states,
                                         uint16_t num_storages) {
    uint8_t *p = buf;
    for (uint16_t i = 0; i < num_storages; i++) {
        const storage_state_t *st = &states[i];
        uint8_t *block_start = p;

        /* Reserve space for checkpoint_block header (8 bytes) */
        p += 8;

        if (st->flags & SF_SPARSE) {
            /* Write valid mask */
            int mask_bytes = (st->num_slots + 7) / 8;
            memset(p, 0, mask_bytes);
            for (uint16_t s = 0; s < st->num_slots; s++) {
                if (st->valid[s])
                    p[s/8] |= (uint8_t)(1 << (s%8));
            }
            p += mask_bytes;

            /* Write data for valid slots */
            for (uint16_t s = 0; s < st->num_slots; s++) {
                if (st->valid[s]) {
                    memcpy(p, st->data + (size_t)s * st->slot_size, st->slot_size);
                    p += st->slot_size;
                }
            }
        } else {
            /* Dense: all slots */
            size_t total = (size_t)st->num_slots * st->slot_size;
            memcpy(p, st->data, total);
            p += total;
        }

        /* Fill in checkpoint_block header */
        uint32_t payload_size = (uint32_t)(p - block_start - 8);
        uint8_t *h = block_start;
        buf_u16(&h, st->storage_id);
        buf_u16(&h, 0); /* reserved */
        buf_u32(&h, payload_size);
    }

    return (uint32_t)(p - buf);
}

/* ── Preamble write ─────────────────────────────────────────────── */

static void write_preamble_chunk(FILE *f, uint16_t type, const uint8_t *payload, uint32_t size) {
    write_u16(f, type);
    write_u16(f, 0); /* flags */
    write_u32(f, size);
    if (size > 0) fwrite(payload, 1, size, f);
    /* Pad to 8-byte alignment */
    int pad = (8 - (size % 8)) % 8;
    if (pad > 0) {
        uint8_t zeros[8] = {0};
        fwrite(zeros, 1, pad, f);
    }
}

/* ── Writer implementation ──────────────────────────────────────── */

uscope_writer_t *uscope_writer_open(const char *path,
                                    const uscope_dut_property_t *props,
                                    uint16_t num_props,
                                    uscope_schema_def_t *schema,
                                    uint64_t checkpoint_interval_ps) {
    FILE *fp = fopen(path, "wb");
    if (!fp) return NULL;

    uscope_writer_t *w = calloc(1, sizeof(*w));
    w->fp = fp;
    w->flags = F_COMPRESSED | F_INTERLEAVED_DELTAS;
    w->checkpoint_interval_ps = checkpoint_interval_ps;

    /* Write file header (placeholder) */
    fwrite(MAGIC, 1, 4, fp);
    write_u16(fp, 0); /* version_major */
    write_u16(fp, 2); /* version_minor */
    write_u64(fp, w->flags);
    write_u64(fp, 0); /* total_time_ps */
    write_u32(fp, 0); /* num_segments */
    write_u32(fp, 0); /* preamble_end */
    write_u64(fp, 0); /* section_table_offset */
    write_u64(fp, 0); /* tail_offset */

    /* Write DUT descriptor chunk */
    {
        /* Build DUT descriptor using schema's string pool */
        uint8_t buf[4096];
        uint8_t *p = buf;
        buf_u16(&p, num_props);
        buf_u16(&p, 0); /* reserved */
        for (uint16_t i = 0; i < num_props; i++) {
            uint16_t key = sp_insert(&schema->sp, props[i].key);
            uint16_t val = sp_insert(&schema->sp, props[i].value);
            buf_u16(&p, key);
            buf_u16(&p, val);
        }
        write_preamble_chunk(fp, CHUNK_DUT_DESC, buf, (uint32_t)(p - buf));
    }

    /* Write schema chunk (serialize to temp file to get size) */
    {
        /* Use an in-memory approach: write schema to a temp buffer via tmpfile */
        FILE *tmp = tmpfile();
        if (!tmp) { free(w); fclose(fp); return NULL; }
        write_schema(tmp, schema);
        long schema_size = ftell(tmp);
        uint8_t *schema_buf = malloc(schema_size);
        fseek(tmp, 0, SEEK_SET);
        fread(schema_buf, 1, schema_size, tmp);
        fclose(tmp);
        write_preamble_chunk(fp, CHUNK_SCHEMA, schema_buf, (uint32_t)schema_size);
        free(schema_buf);
    }

    /* Write trace config chunk */
    {
        uint8_t buf[8];
        uint8_t *p = buf;
        buf_u64(&p, checkpoint_interval_ps);
        write_preamble_chunk(fp, CHUNK_TRACE_CONFIG, buf, 8);
    }

    /* Write end chunk */
    write_preamble_chunk(fp, CHUNK_END, NULL, 0);

    w->preamble_end = (uint32_t)ftell(fp);

    /* Update header with preamble_end */
    fseek(fp, 0, SEEK_SET);
    fwrite(MAGIC, 1, 4, fp);
    write_u16(fp, 0);
    write_u16(fp, 2);
    write_u64(fp, w->flags);
    write_u64(fp, 0);
    write_u32(fp, 0);
    write_u32(fp, w->preamble_end);
    write_u64(fp, 0);
    write_u64(fp, 0);
    fseek(fp, 0, SEEK_END);

    /* Initialize storage states */
    w->num_storages = schema->num_storages;
    w->states = calloc(schema->num_storages, sizeof(storage_state_t));
    for (uint16_t i = 0; i < schema->num_storages; i++) {
        state_init(&w->states[i], &schema->storages[i]);
    }

    /* Allocate delta buffer */
    w->delta_buf_cap = MAX_DELTA_BUF;
    w->delta_buf = malloc(w->delta_buf_cap);
    w->delta_buf_len = 0;

    /* Allocate segment index */
    w->seg_index = calloc(MAX_SEGMENTS, sizeof(seg_index_entry_t));
    w->seg_index_len = 0;

    /* String table */
    w->strings_cap = 1024;
    w->strings = calloc(w->strings_cap, sizeof(string_entry_t));
    w->num_strings = 0;

    w->next_checkpoint_ps = checkpoint_interval_ps;

    uscope_schema_free(schema);
    return w;
}

/* ── Segment flush ──────────────────────────────────────────────── */

static void flush_segment(uscope_writer_t *w) {
    if (w->delta_buf_len == 0 && w->seg_num_frames == 0) return;

    uint64_t seg_offset = (uint64_t)ftell(w->fp);

    /* Build checkpoint */
    uint8_t *ckpt_buf = malloc(4*1024*1024);
    uint32_t ckpt_size = write_checkpoint_to_buf(ckpt_buf, w->states, w->num_storages);

    /* LZ4 compress delta blob */
    int max_compressed = LZ4_compressBound((int)w->delta_buf_len);
    /* Prepend uncompressed size (4 bytes LE) like lz4_flex::compress_prepend_size */
    uint8_t *compressed_buf = malloc(4 + max_compressed);
    compressed_buf[0] = (uint8_t)(w->delta_buf_len);
    compressed_buf[1] = (uint8_t)(w->delta_buf_len >> 8);
    compressed_buf[2] = (uint8_t)(w->delta_buf_len >> 16);
    compressed_buf[3] = (uint8_t)(w->delta_buf_len >> 24);
    int compressed_size = LZ4_compress_default(
        (const char*)w->delta_buf, (char*)compressed_buf + 4,
        (int)w->delta_buf_len, max_compressed);
    uint32_t total_compressed = (uint32_t)(4 + compressed_size);

    /* Write segment header (56 bytes) */
    fwrite(SEG_MAGIC, 1, 4, w->fp);
    write_u32(w->fp, 0); /* flags */
    write_u64(w->fp, w->seg_time_start);
    write_u64(w->fp, w->seg_time_end);
    write_u64(w->fp, w->prev_seg_offset);
    write_u32(w->fp, ckpt_size);
    write_u32(w->fp, total_compressed);
    write_u32(w->fp, w->delta_buf_len);
    write_u32(w->fp, w->seg_num_frames);
    write_u32(w->fp, w->seg_num_frames_active);
    write_u32(w->fp, 0); /* reserved */

    /* Write checkpoint data */
    fwrite(ckpt_buf, 1, ckpt_size, w->fp);

    /* Write compressed deltas */
    fwrite(compressed_buf, 1, total_compressed, w->fp);

    free(ckpt_buf);
    free(compressed_buf);

    /* Track segment */
    if (w->seg_index_len < MAX_SEGMENTS) {
        w->seg_index[w->seg_index_len].offset = seg_offset;
        w->seg_index[w->seg_index_len].time_start_ps = w->seg_time_start;
        w->seg_index[w->seg_index_len].time_end_ps = w->seg_time_end;
        w->seg_index_len++;
    }

    w->prev_seg_offset = seg_offset;
    w->num_segments++;
    w->tail_offset = seg_offset;

    /* Update file header on disk */
    long cur = ftell(w->fp);
    fseek(w->fp, 0, SEEK_SET);
    fwrite(MAGIC, 1, 4, w->fp);
    write_u16(w->fp, 0);
    write_u16(w->fp, 2);
    write_u64(w->fp, w->flags);
    write_u64(w->fp, w->total_time_ps);
    write_u32(w->fp, w->num_segments);
    write_u32(w->fp, w->preamble_end);
    write_u64(w->fp, w->section_table_offset);
    write_u64(w->fp, w->tail_offset);
    fseek(w->fp, cur, SEEK_SET);

    /* Reset delta buffer */
    w->delta_buf_len = 0;
    w->seg_time_start = w->current_time_ps;
    w->seg_time_end = w->current_time_ps;
    w->seg_num_frames = 0;
    w->seg_num_frames_active = 0;
    w->last_frame_time_ps = w->current_time_ps;
}

/* ── Per-cycle API ──────────────────────────────────────────────── */

void uscope_begin_cycle(uscope_writer_t *w, uint64_t time_ps) {
    assert(!w->in_cycle);
    assert(time_ps >= w->current_time_ps);
    w->current_time_ps = time_ps;
    w->in_cycle = 1;
    w->num_ops = 0;
    w->num_events = 0;
    w->num_items = 0;
}

void uscope_slot_set(uscope_writer_t *w, uint16_t storage_id,
                     uint16_t slot, uint16_t field, uint64_t value) {
    assert(w->in_cycle);
    if (w->num_ops < MAX_OPS_PER_FRAME) {
        uint16_t idx = w->num_ops++;
        delta_op_t *op = &w->ops[idx];
        op->action = DA_SLOT_SET;
        op->storage_id = storage_id;
        op->slot_index = slot;
        op->field_index = field;
        op->value = value;
        w->item_order[w->num_items++] = idx; /* op index, bit 15 clear */
    }
    if (storage_id < w->num_storages)
        state_set_field(&w->states[storage_id], slot, field, value);
}

void uscope_slot_clear(uscope_writer_t *w, uint16_t storage_id,
                       uint16_t slot) {
    assert(w->in_cycle);
    if (w->num_ops < MAX_OPS_PER_FRAME) {
        uint16_t idx = w->num_ops++;
        delta_op_t *op = &w->ops[idx];
        op->action = DA_SLOT_CLEAR;
        op->storage_id = storage_id;
        op->slot_index = slot;
        op->field_index = 0;
        op->value = 0;
        w->item_order[w->num_items++] = idx;
    }
    if (storage_id < w->num_storages)
        state_clear_slot(&w->states[storage_id], slot);
}

void uscope_slot_add(uscope_writer_t *w, uint16_t storage_id,
                     uint16_t slot, uint16_t field, uint64_t value) {
    assert(w->in_cycle);
    if (w->num_ops < MAX_OPS_PER_FRAME) {
        uint16_t idx = w->num_ops++;
        delta_op_t *op = &w->ops[idx];
        op->action = DA_SLOT_ADD;
        op->storage_id = storage_id;
        op->slot_index = slot;
        op->field_index = field;
        op->value = value;
        w->item_order[w->num_items++] = idx;
    }
    if (storage_id < w->num_storages)
        state_add_field(&w->states[storage_id], slot, field, value);
}

void uscope_event(uscope_writer_t *w, uint16_t event_type_id,
                  const void *payload, uint32_t payload_size) {
    assert(w->in_cycle);
    if (w->num_events < MAX_EVENTS_PER_FRAME && payload_size <= 256) {
        uint16_t idx = w->num_events++;
        event_rec_t *ev = &w->events[idx];
        ev->event_type_id = event_type_id;
        ev->payload_size = payload_size;
        if (payload_size > 0)
            memcpy(ev->payload, payload, payload_size);
        w->item_order[w->num_items++] = (1u << 15) | idx; /* event, bit 15 set */
    }
}

void uscope_end_cycle(uscope_writer_t *w) {
    assert(w->in_cycle);
    w->in_cycle = 0;

    uint64_t time_delta = w->current_time_ps - w->last_frame_time_ps;
    w->last_frame_time_ps = w->current_time_ps;
    w->seg_time_end = w->current_time_ps;
    w->seg_num_frames++;
    if (w->num_ops > 0 || w->num_events > 0)
        w->seg_num_frames_active++;

    /* Check if all ops fit compact format */
    int use_compact = 1;
    for (uint16_t i = 0; i < w->num_ops; i++) {
        if (w->ops[i].storage_id > 255 || w->ops[i].value > 65535) {
            use_compact = 0;
            break;
        }
    }

    /* Ensure delta buffer has space */
    uint32_t needed = 10 + 2 + (uint32_t)w->num_items * 17 +
                      (uint32_t)w->num_events * 264;
    if (w->delta_buf_len + needed > w->delta_buf_cap) {
        w->delta_buf_cap *= 2;
        w->delta_buf = realloc(w->delta_buf, w->delta_buf_cap);
    }

    uint8_t *p = w->delta_buf + w->delta_buf_len;

    /* LEB128 time delta */
    p += leb128_encode(time_delta, p);

    /* v0.2: num_items */
    buf_u16(&p, w->num_items);

    /* Items in insertion order */
    for (uint16_t i = 0; i < w->num_items; i++) {
        uint16_t entry = w->item_order[i];
        int is_event = (entry >> 15) & 1;
        uint16_t idx = entry & 0x7FFF;

        if (is_event) {
            const event_rec_t *ev = &w->events[idx];
            *p++ = TAG_EVENT;
            *p++ = 0; /* reserved */
            buf_u16(&p, ev->event_type_id);
            buf_u32(&p, ev->payload_size);
            memcpy(p, ev->payload, ev->payload_size);
            p += ev->payload_size;
        } else {
            const delta_op_t *op = &w->ops[idx];
            if (use_compact) {
                *p++ = TAG_COMPACT_OP;
                *p++ = op->action;
                *p++ = (uint8_t)op->storage_id;
                buf_u16(&p, op->slot_index);
                buf_u16(&p, op->field_index);
                buf_u16(&p, (uint16_t)op->value);
            } else {
                *p++ = TAG_WIDE_OP;
                *p++ = op->action;
                buf_u16(&p, op->storage_id);
                buf_u16(&p, op->slot_index);
                buf_u16(&p, op->field_index);
                buf_u64(&p, op->value);
            }
        }
    }

    w->delta_buf_len = (uint32_t)(p - w->delta_buf);

    /* Check if we should flush a segment */
    if (w->current_time_ps >= w->next_checkpoint_ps) {
        flush_segment(w);
        w->next_checkpoint_ps = w->current_time_ps + w->checkpoint_interval_ps;
    }
}

/* ── String table ───────────────────────────────────────────────── */

uint32_t uscope_string_insert(uscope_writer_t *w, const char *str) {
    /* Dedup check */
    for (uint32_t i = 0; i < w->num_strings; i++) {
        if (strcmp(w->strings[i].str, str) == 0) return i;
    }
    if (w->num_strings >= w->strings_cap) {
        w->strings_cap *= 2;
        w->strings = realloc(w->strings, w->strings_cap * sizeof(string_entry_t));
    }
    uint32_t idx = w->num_strings++;
    w->strings[idx].str = strdup(str);
    w->strings[idx].len = (uint32_t)strlen(str);
    return idx;
}

/* ── Writer close / finalization ────────────────────────────────── */

void uscope_writer_close(uscope_writer_t *w) {
    if (!w) return;

    /* Flush any remaining data */
    if (w->delta_buf_len > 0 || w->seg_num_frames > 0) {
        flush_segment(w);
    }

    /* === Write string table section === */
    uint64_t string_table_offset = (uint64_t)ftell(w->fp);
    uint64_t string_table_size = 0;
    int has_strings = (w->num_strings > 0);

    if (has_strings) {
        /* Header: num_entries(4) + reserved(4) */
        write_u32(w->fp, w->num_strings);
        write_u32(w->fp, 0);
        string_table_size += 8;

        /* Compute data offsets */
        uint32_t data_offset = 0;
        for (uint32_t i = 0; i < w->num_strings; i++) {
            write_u32(w->fp, data_offset);
            write_u32(w->fp, w->strings[i].len);
            string_table_size += 8;
            data_offset += w->strings[i].len + 1; /* +1 for null */
        }

        /* Write string data */
        for (uint32_t i = 0; i < w->num_strings; i++) {
            fwrite(w->strings[i].str, 1, w->strings[i].len, w->fp);
            write_u8(w->fp, 0); /* null terminator */
            string_table_size += w->strings[i].len + 1;
        }

        w->flags |= F_HAS_STRINGS;
    }

    /* === Write segment table section === */
    uint64_t seg_table_offset = (uint64_t)ftell(w->fp);
    uint64_t seg_table_size = 0;
    if (w->seg_index_len > 0) {
        for (uint32_t i = 0; i < w->seg_index_len; i++) {
            write_u64(w->fp, w->seg_index[i].offset);
            write_u64(w->fp, w->seg_index[i].time_start_ps);
            write_u64(w->fp, w->seg_index[i].time_end_ps);
            seg_table_size += 24;
        }
    }

    /* === Pad to 8-byte alignment === */
    long pos = ftell(w->fp);
    int pad = (8 - (pos % 8)) % 8;
    if (pad > 0) {
        uint8_t zeros[8] = {0};
        fwrite(zeros, 1, pad, w->fp);
    }

    /* === Write section table === */
    w->section_table_offset = (uint64_t)ftell(w->fp);

    if (has_strings) {
        write_u16(w->fp, SECTION_STRINGS);
        write_u16(w->fp, 0);
        write_u32(w->fp, 0);
        write_u64(w->fp, string_table_offset);
        write_u64(w->fp, string_table_size);
    }

    if (w->seg_index_len > 0) {
        write_u16(w->fp, SECTION_SEGMENTS);
        write_u16(w->fp, 0);
        write_u32(w->fp, 0);
        write_u64(w->fp, seg_table_offset);
        write_u64(w->fp, seg_table_size);
    }

    /* End sentinel */
    write_u16(w->fp, SECTION_END);
    write_u16(w->fp, 0);
    write_u32(w->fp, 0);
    write_u64(w->fp, 0);
    write_u64(w->fp, 0);

    /* === Update file header === */
    w->flags |= F_COMPLETE;
    w->total_time_ps = w->current_time_ps;

    fseek(w->fp, 0, SEEK_SET);
    fwrite(MAGIC, 1, 4, w->fp);
    write_u16(w->fp, 0);
    write_u16(w->fp, 2);
    write_u64(w->fp, w->flags);
    write_u64(w->fp, w->total_time_ps);
    write_u32(w->fp, w->num_segments);
    write_u32(w->fp, w->preamble_end);
    write_u64(w->fp, w->section_table_offset);
    write_u64(w->fp, w->tail_offset);

    fclose(w->fp);

    /* Free storage states */
    for (uint16_t i = 0; i < w->num_storages; i++)
        state_free(&w->states[i]);
    free(w->states);

    /* Free strings */
    for (uint32_t i = 0; i < w->num_strings; i++)
        free(w->strings[i].str);
    free(w->strings);

    free(w->delta_buf);
    free(w->seg_index);
    free(w);
}
