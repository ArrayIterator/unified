// Diesel schema for the payments domain of the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    store_coupons (id) {
        id -> Uuid,
        code -> Varchar,
        discount_type -> Varchar,
        discount_value -> Numeric,
        expires_at -> Nullable<Timestamptz>,
        usage_limit -> Nullable<Int4>,
        used_count -> Int4,
        product_ids -> Jsonb,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    billing_settings (key) {
        key -> Varchar,
        invoice_prefix -> Varchar,
        default_tax_rate -> Float8,
        tax_inclusive -> Bool,
        payment_terms_days -> Int4,
        reminder_before_days -> Jsonb,
        reminder_after_days -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    invoice_sequences (name) {
        name -> Varchar,
        next_value -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// Payments and bank transfer (2026-11-22-000014_payments_bank_transfer).

diesel::table! {
    payment_connections (id) {
        id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        mode -> Varchar,
        endpoint -> Nullable<Varchar>,
        credentials -> Jsonb,
        custom_config -> Nullable<Jsonb>,
        delegate_connection_id -> Nullable<Uuid>,
        enabled -> Bool,
        last_tested_at -> Nullable<Timestamptz>,
        last_status -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    payment_intents (id) {
        id -> Uuid,
        connection_id -> Uuid,
        provider -> Varchar,
        customer_id -> Varchar,
        // C11 (migration 000126): tenant stamp. NULL for rows
        // written before stamping; the runtime records carry no
        // tenant_id field yet, so the write-through stamps it.
        tenant_id -> Nullable<Varchar>,
        order_id -> Nullable<Varchar>,
        invoice_id -> Nullable<Varchar>,
        amount_minor -> Int8,
        currency -> Varchar,
        status -> Varchar,
        capture_method -> Varchar,
        provider_transaction_id -> Nullable<Varchar>,
        fee_minor -> Nullable<Int8>,
        idempotency_key -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    payment_transactions (id) {
        id -> Uuid,
        intent_id -> Nullable<Uuid>,
        connection_id -> Uuid,
        provider -> Varchar,
        kind -> Varchar,
        customer_id -> Varchar,
        // C11 (migration 000126): tenant stamp, see
        // `payment_intents.tenant_id`.
        tenant_id -> Nullable<Varchar>,
        order_id -> Nullable<Varchar>,
        invoice_id -> Nullable<Varchar>,
        subscription_id -> Nullable<Uuid>,
        amount_minor -> Int8,
        currency -> Varchar,
        fee_minor -> Nullable<Int8>,
        status -> Varchar,
        provider_transaction_id -> Varchar,
        provider_refund_id -> Nullable<Varchar>,
        idempotency_key -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    payment_methods (id) {
        id -> Uuid,
        customer_id -> Varchar,
        connection_id -> Uuid,
        provider -> Varchar,
        provider_token -> Varchar,
        brand -> Varchar,
        last4 -> Varchar,
        exp_month -> Int4,
        exp_year -> Int4,
        is_default -> Bool,
        // C11 (migration 000126): tenant stamp, see
        // `payment_intents.tenant_id`.
        tenant_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gateway_subscriptions (id) {
        id -> Uuid,
        connection_id -> Uuid,
        provider -> Varchar,
        customer_id -> Varchar,
        // C11 (migration 000126): tenant stamp, see
        // `payment_intents.tenant_id`.
        tenant_id -> Nullable<Varchar>,
        order_id -> Nullable<Varchar>,
        plan_id -> Nullable<Varchar>,
        amount_minor -> Int8,
        currency -> Varchar,
        interval -> Varchar,
        interval_count -> Int4,
        status -> Varchar,
        provider_subscription_id -> Varchar,
        payment_method_id -> Nullable<Uuid>,
        current_period_end -> Nullable<Timestamptz>,
        next_renewal_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    split_bills (id) {
        id -> Uuid,
        order_id -> Nullable<Varchar>,
        invoice_id -> Nullable<Varchar>,
        customer_id -> Varchar,
        total_minor -> Int8,
        currency -> Varchar,
        status -> Varchar,
        paid_minor -> Int8,
        remaining_minor -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    split_parts (id) {
        id -> Uuid,
        split_id -> Uuid,
        index -> Int4,
        amount_minor -> Int8,
        currency -> Varchar,
        provider -> Varchar,
        connection_id -> Nullable<Uuid>,
        payer_id -> Varchar,
        due_date -> Nullable<Timestamptz>,
        status -> Varchar,
        paid_transaction_id -> Nullable<Uuid>,
        paid_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    bank_accounts (id) {
        id -> Uuid,
        bank_name -> Varchar,
        account_holder -> Varchar,
        account_number -> Varchar,
        swift_bic -> Nullable<Varchar>,
        currency -> Varchar,
        instructions -> Nullable<Text>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    bank_transfer_verifications (id) {
        id -> Uuid,
        order_id -> Varchar,
        invoice_id -> Nullable<Varchar>,
        customer_id -> Varchar,
        split_id -> Nullable<Uuid>,
        part_id -> Nullable<Uuid>,
        bank_account_id -> Nullable<Uuid>,
        status -> Varchar,
        expected_amount_minor -> Nullable<Int8>,
        currency -> Varchar,
        proof -> Nullable<Jsonb>,
        amount_check -> Varchar,
        amount_difference_minor -> Nullable<Int8>,
        transaction_id -> Nullable<Varchar>,
        verified_by -> Nullable<Varchar>,
        verified_at -> Nullable<Timestamptz>,
        rejection_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    bank_transfer_verification_audit (id) {
        id -> Uuid,
        verification_id -> Uuid,
        action -> Varchar,
        actor -> Varchar,
        actor_role -> Varchar,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    bank_transfer_settings (key) {
        key -> Varchar,
        amount_tolerance_minor -> Int8,
        proof_expiry_days -> Int4,
        require_reference_number -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
