CREATE TABLE refunds (
    id INTEGER PRIMARY KEY NOT NULL,
    reserve_id TEXT NOT NULL,
    reserve_box_id TEXT,
    requested_amount BIGINT NOT NULL,
    refunded_amount BIGINT,
    init_height INTEGER NOT NULL,
    settled_height INTEGER,
    status TEXT CHECK (status IN ('pending', 'executing', 'completed', 'cancelled', 'failed')) NOT NULL,
    init_tx_id TEXT,
    refund_tx_id TEXT,
    error_message TEXT
);

CREATE INDEX refunds_reserve_id_idx ON refunds(reserve_id);
CREATE INDEX refunds_status_idx ON refunds(status);

CREATE UNIQUE INDEX refunds_active_reserve_idx
    ON refunds(reserve_id)
    WHERE status IN ('pending', 'executing');
