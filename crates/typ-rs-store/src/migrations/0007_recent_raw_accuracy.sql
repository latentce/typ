-- Raw accuracy joins the recent series cached with each summary. NULL in
-- rows written before this column existed, until a rebuild recomputes them;
-- the model version is unchanged, so no rebuild is forced.
ALTER TABLE session_metrics ADD COLUMN recent_raw_accuracy REAL;
