/*
 * µScope DPI Bridge — C writer library for hardware simulators.
 *
 * SPDX-License-Identifier: Apache-2.0
 *
 * This is a standalone C99 library with no Rust dependencies.
 * It produces µScope trace files readable by the Rust reader.
 */

#ifndef USCOPE_DPI_H
#define USCOPE_DPI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Opaque types ───────────────────────────────────────────────── */

typedef struct uscope_writer uscope_writer_t;
typedef struct uscope_schema uscope_schema_def_t;

/* ── DUT properties ─────────────────────────────────────────────── */

typedef struct {
  const char *key;
  const char *value;
} uscope_dut_property_t;

/* ── Field types (matches spec) ─────────────────────────────────── */

#define USCOPE_FT_U8         0x01
#define USCOPE_FT_U16        0x02
#define USCOPE_FT_U32        0x03
#define USCOPE_FT_U64        0x04
#define USCOPE_FT_I8         0x05
#define USCOPE_FT_I16        0x06
#define USCOPE_FT_I32        0x07
#define USCOPE_FT_I64        0x08
#define USCOPE_FT_BOOL       0x09
#define USCOPE_FT_STRING_REF 0x0A
#define USCOPE_FT_ENUM       0x0B

/* ── Storage flags ──────────────────────────────────────────────── */

#define USCOPE_SF_SPARSE 0x0001
#define USCOPE_SF_BUFFER 0x0002

/* ── Schema building ────────────────────────────────────────────── */

/* Create a new empty schema definition. */
uscope_schema_def_t *uscope_schema_new(void);

/* Free a schema definition (not needed after uscope_writer_open). */
void uscope_schema_free(uscope_schema_def_t *s);

/* Add a clock domain. Returns clock_id. */
uint8_t uscope_schema_add_clock(uscope_schema_def_t *s, const char *name, uint32_t period_ps);

/* Add a scope. parent=0xFFFF for root. Returns scope_id. */
uint16_t uscope_schema_add_scope(uscope_schema_def_t *s, const char *name, uint16_t parent, const char *protocol,
                                 uint8_t clock_id);

/* Add an enum type. Returns enum_id. */
uint8_t uscope_schema_add_enum(uscope_schema_def_t *s, const char *name, const char **values, uint8_t count);

/*
 * Add a storage. Fields are given as parallel arrays of (name, type, enum_id).
 * Returns storage_id.
 */
uint16_t uscope_schema_add_storage(uscope_schema_def_t *s, const char *name, uint16_t scope_id, uint16_t num_slots,
                                   uint16_t flags, uint16_t num_fields, const char **field_names,
                                   const uint8_t *field_types, const uint8_t *field_enum_ids);

/*
 * Add a storage with properties (v0.3). Returns storage_id.
 * Properties are scalar values attached to the storage, not per-slot.
 * prop_roles: 0=plain, 1=HEAD_PTR, 2=TAIL_PTR (NULL = all plain).
 * prop_pair_ids: pointer pair grouping (NULL = all 0).
 */
#define USCOPE_PROP_ROLE_PLAIN    0
#define USCOPE_PROP_ROLE_HEAD_PTR 1
#define USCOPE_PROP_ROLE_TAIL_PTR 2
uint16_t uscope_schema_add_storage_with_properties(
    uscope_schema_def_t *s, const char *name, uint16_t scope_id, uint16_t num_slots,
    uint16_t flags, uint16_t num_fields, const char **field_names,
    const uint8_t *field_types, const uint8_t *field_enum_ids,
    uint16_t num_properties, const char **prop_names,
    const uint8_t *prop_types, const uint8_t *prop_enum_ids,
    const uint8_t *prop_roles, const uint8_t *prop_pair_ids);

/*
 * Add an event type. Fields given as parallel arrays.
 * Returns event_type_id.
 */
uint16_t uscope_schema_add_event(uscope_schema_def_t *s, const char *name, uint16_t scope_id, uint16_t num_fields,
                                 const char **field_names, const uint8_t *field_types, const uint8_t *field_enum_ids);

/* ── Writer lifecycle ───────────────────────────────────────────── */

/*
 * Open a new trace file for writing.
 * Takes ownership of the schema (schema is freed internally).
 */
uscope_writer_t *uscope_writer_open(const char *path, const uscope_dut_property_t *props, uint16_t num_props,
                                    uscope_schema_def_t *schema, uint64_t checkpoint_interval_ps);

/* Finalize and close the trace file. */
void uscope_writer_close(uscope_writer_t *w);

/* ── Per-cycle operations ───────────────────────────────────────── */

void uscope_begin_cycle(uscope_writer_t *w, uint64_t time_ps);

void uscope_slot_set(uscope_writer_t *w, uint16_t storage_id, uint16_t slot, uint16_t field, uint64_t value);

void uscope_slot_clear(uscope_writer_t *w, uint16_t storage_id, uint16_t slot);

void uscope_slot_add(uscope_writer_t *w, uint16_t storage_id, uint16_t slot, uint16_t field, uint64_t value);

void uscope_event(uscope_writer_t *w, uint16_t event_type_id, const void *payload, uint32_t payload_size);

void uscope_end_cycle(uscope_writer_t *w);

/* Set a storage-level property (v0.3). */
void uscope_prop_set(uscope_writer_t *w, uint16_t storage_id, uint16_t prop_index, uint64_t value);

/* ── String table ───────────────────────────────────────────────── */

/* Insert a string into the string table. Returns its index for STRING_REF fields. */
uint32_t uscope_string_insert(uscope_writer_t *w, const char *str);

#ifdef __cplusplus
}
#endif

#endif /* USCOPE_DPI_H */
