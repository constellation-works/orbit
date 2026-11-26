/// The mode, window, write batches and store pages shared by the store reports.
pub(super) fn summary_line(doc: &serde_json::Value) -> String {
    let field = |pointer: &str| {
        doc.pointer(pointer)
            .map(|value| {
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_string)
            })
            .unwrap_or_default()
    };
    format!(
        "mode={} retention_days={} cutoff={} batches={} max_batch_ms={} store_bytes={} \
         freelist_bytes={}",
        if doc["apply"] == true {
            "apply"
        } else {
            "plan"
        },
        field("/retention_days"),
        field("/cutoff"),
        field("/writes/batches"),
        field("/writes/max_batch_ms"),
        field("/store/file_bytes"),
        field("/store/freelist_bytes"),
    )
}
