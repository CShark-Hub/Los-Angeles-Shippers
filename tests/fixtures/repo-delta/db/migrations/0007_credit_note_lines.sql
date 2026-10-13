CREATE TABLE credit_note_lines (
    credit_note_line_id BIGINT PRIMARY KEY,
    invoice_ledger_id BIGINT NOT NULL REFERENCES invoice_ledger (invoice_ledger_id),
    refunded_amount NUMERIC(12, 2) NOT NULL,
    refund_reason_code VARCHAR(32) NOT NULL,
    created_at_utc TIMESTAMP NOT NULL
);

CREATE INDEX credit_note_lines_by_invoice ON credit_note_lines (invoice_ledger_id);
