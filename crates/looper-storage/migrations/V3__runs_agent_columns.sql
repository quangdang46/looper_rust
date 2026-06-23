-- Add agent_vendor and model columns to runs so that the API can persist
-- and return the vendor/model used to start a run, instead of always
-- returning an empty string.

ALTER TABLE runs ADD COLUMN agent_vendor TEXT;
ALTER TABLE runs ADD COLUMN model TEXT;