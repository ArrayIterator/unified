// Diesel schema for the printing domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Printing (migration 000091_printing).
// ---------------------------------------------------------------------------

diesel::table! {
    paper_sizes (code) {
        code -> Varchar,
        name -> Varchar,
        width_mm -> Numeric,
        height_mm -> Nullable<Numeric>,
        margin_top_mm -> Numeric,
        margin_bottom_mm -> Numeric,
        margin_left_mm -> Numeric,
        margin_right_mm -> Numeric,
        is_standard -> Bool,
        is_thermal -> Bool,
        thermal_width_mm -> Nullable<Int4>,
        company_id -> Nullable<Varchar>,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoice_templates (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        name -> Varchar,
        is_default -> Bool,
        paper_size_code -> Nullable<Varchar>,
        header_text -> Nullable<Text>,
        footer_text -> Nullable<Text>,
        logo_storage_target -> Nullable<Varchar>,
        logo_storage_key -> Nullable<Varchar>,
        logo_url -> Nullable<Varchar>,
        primary_color -> Varchar,
        accent_color -> Varchar,
        font_family -> Varchar,
        #[sql_name = "columns"]
        columns_ -> Jsonb,
        line_item_layout -> Jsonb,
        show_tax_breakdown -> Bool,
        payment_instructions -> Nullable<Text>,
        footer_notes -> Nullable<Text>,
        version -> Int4,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoice_template_versions (id) {
        id -> Uuid,
        template_id -> Uuid,
        version -> Int4,
        snapshot -> Jsonb,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    printer_profiles (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        name -> Varchar,
        connection_type -> Varchar,
        connection_target -> Nullable<Varchar>,
        auth_token_ref -> Nullable<Varchar>,
        width_columns -> Int4,
        character_set -> Varchar,
        code_page -> Varchar,
        supports_drawer_kick -> Bool,
        supports_cut -> Bool,
        paper_size_code -> Nullable<Varchar>,
        is_default -> Bool,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    print_jobs (id) {
        id -> Uuid,
        company_id -> Nullable<Varchar>,
        invoice_id -> Varchar,
        template_id -> Nullable<Uuid>,
        template_version -> Nullable<Int4>,
        paper_size_code -> Nullable<Varchar>,
        output_kind -> Varchar,
        printer_profile_id -> Nullable<Uuid>,
        status -> Varchar,
        document_ref -> Nullable<Varchar>,
        escpos_preview -> Nullable<Text>,
        error_message -> Nullable<Text>,
        attempts -> Int4,
        requested_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        started_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
    }
}
