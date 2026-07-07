-- Source of truth: append-only, immutable event log.
CREATE TABLE IF NOT EXISTS events (
  id         TEXT PRIMARY KEY,
  hlc        TEXT NOT NULL,
  device_id  TEXT NOT NULL,
  user_id    TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  type       TEXT NOT NULL,
  payload    BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE (device_id, seq)
);
CREATE INDEX IF NOT EXISTS events_hlc ON events (hlc);

-- Replay bookmark: how far each projection has been applied.
CREATE TABLE IF NOT EXISTS projection_cursor (
  projection TEXT PRIMARY KEY,
  last_hlc   TEXT NOT NULL
);
