/*
 * Test program: write a simple uscope trace using the C DPI library.
 * The output should be readable by the Rust reader.
 */

#include "uscope_dpi.h"
#include <stdio.h>
#include <string.h>

int main(void) {
    /* Build schema */
    uscope_schema_def_t *schema = uscope_schema_new();

    uint8_t clk = uscope_schema_add_clock(schema, "core_clk", 1000);
    uscope_schema_add_scope(schema, "root", 0xFFFF, NULL, 0xFF);
    uint16_t scope_id = uscope_schema_add_scope(schema, "core0", 0, "cpu", clk);

    const char *stages[] = {"fetch", "decode", "execute", "writeback"};
    uint8_t stage_enum = uscope_schema_add_enum(schema, "pipeline_stage", stages, 4);

    /* Entity catalog */
    const char *entity_fields[] = {"entity_id", "pc", "inst_bits"};
    uint8_t entity_types[] = {USCOPE_FT_U32, USCOPE_FT_U64, USCOPE_FT_U32};
    uint8_t entity_enums[] = {0, 0, 0};
    uint16_t entities_id = uscope_schema_add_storage(
        schema, "entities", scope_id, 16, USCOPE_SF_SPARSE,
        3, entity_fields, entity_types, entity_enums);

    /* Counter */
    const char *counter_fields[] = {"count"};
    uint8_t counter_types[] = {USCOPE_FT_U64};
    uint16_t counter_id = uscope_schema_add_storage(
        schema, "committed_insns", scope_id, 1, 0,
        1, counter_fields, counter_types, NULL);

    /* Stage transition event */
    const char *st_fields[] = {"entity_id", "stage"};
    uint8_t st_types[] = {USCOPE_FT_U32, USCOPE_FT_ENUM};
    uint8_t st_enums[] = {0, stage_enum};
    uint16_t st_event = uscope_schema_add_event(
        schema, "stage_transition", scope_id,
        2, st_fields, st_types, st_enums);

    /* Annotate event */
    const char *ann_fields[] = {"entity_id", "text"};
    uint8_t ann_types[] = {USCOPE_FT_U32, USCOPE_FT_STRING_REF};
    uint16_t ann_event = uscope_schema_add_event(
        schema, "annotate", scope_id,
        2, ann_fields, ann_types, NULL);

    /* DUT properties */
    uscope_dut_property_t props[] = {
        {"dut_name", "test_core_c"},
        {"cpu.isa", "RV64GC"},
        {"cpu.pipeline_stages", "fetch,decode,execute,writeback"},
    };

    /* Open writer */
    uscope_writer_t *w = uscope_writer_open(
        "test_output.uscope", props, 3, schema, 10000);

    if (!w) {
        fprintf(stderr, "Failed to open writer\n");
        return 1;
    }

    /* Write a simple trace: 2 instructions through 4 stages */

    /* Cycle 0: fetch inst 0 */
    uscope_begin_cycle(w, 0);
    uscope_slot_set(w, entities_id, 0, 0, 0);          /* entity_id = 0 */
    uscope_slot_set(w, entities_id, 0, 1, 0x80000000); /* pc */
    uscope_slot_set(w, entities_id, 0, 2, 0x13);       /* inst_bits = nop */
    {
        uint8_t payload[5];
        uint32_t eid = 0; memcpy(payload, &eid, 4);
        payload[4] = 0; /* fetch */
        uscope_event(w, st_event, payload, 5);
    }
    {
        uint32_t text_ref = uscope_string_insert(w, "addi x0, x0, 0");
        uint8_t payload[8];
        uint32_t eid = 0; memcpy(payload, &eid, 4);
        memcpy(payload + 4, &text_ref, 4);
        uscope_event(w, ann_event, payload, 8);
    }
    uscope_end_cycle(w);

    /* Cycle 1: decode inst 0, fetch inst 1 */
    uscope_begin_cycle(w, 1000);
    {
        uint8_t payload[5];
        uint32_t eid = 0; memcpy(payload, &eid, 4);
        payload[4] = 1; /* decode */
        uscope_event(w, st_event, payload, 5);
    }
    uscope_slot_set(w, entities_id, 1, 0, 1);          /* entity_id = 1 */
    uscope_slot_set(w, entities_id, 1, 1, 0x80000004); /* pc */
    uscope_slot_set(w, entities_id, 1, 2, 0x93);       /* inst_bits */
    {
        uint8_t payload[5];
        uint32_t eid = 1; memcpy(payload, &eid, 4);
        payload[4] = 0; /* fetch */
        uscope_event(w, st_event, payload, 5);
    }
    uscope_end_cycle(w);

    /* Cycle 2: execute inst 0, decode inst 1 */
    uscope_begin_cycle(w, 2000);
    {
        uint8_t payload[5];
        uint32_t eid = 0; memcpy(payload, &eid, 4);
        payload[4] = 2; /* execute */
        uscope_event(w, st_event, payload, 5);
    }
    {
        uint8_t payload[5];
        uint32_t eid = 1; memcpy(payload, &eid, 4);
        payload[4] = 1; /* decode */
        uscope_event(w, st_event, payload, 5);
    }
    uscope_end_cycle(w);

    /* Cycle 3: writeback+retire inst 0, execute inst 1 */
    uscope_begin_cycle(w, 3000);
    {
        uint8_t payload[5];
        uint32_t eid = 0; memcpy(payload, &eid, 4);
        payload[4] = 3; /* writeback */
        uscope_event(w, st_event, payload, 5);
    }
    uscope_slot_clear(w, entities_id, 0);
    uscope_slot_add(w, counter_id, 0, 0, 1);
    {
        uint8_t payload[5];
        uint32_t eid = 1; memcpy(payload, &eid, 4);
        payload[4] = 2; /* execute */
        uscope_event(w, st_event, payload, 5);
    }
    uscope_end_cycle(w);

    /* Cycle 4: writeback+retire inst 1 */
    uscope_begin_cycle(w, 4000);
    {
        uint8_t payload[5];
        uint32_t eid = 1; memcpy(payload, &eid, 4);
        payload[4] = 3; /* writeback */
        uscope_event(w, st_event, payload, 5);
    }
    uscope_slot_clear(w, entities_id, 1);
    uscope_slot_add(w, counter_id, 0, 0, 1);
    uscope_end_cycle(w);

    uscope_writer_close(w);

    printf("C writer test: wrote test_output.uscope\n");
    return 0;
}
