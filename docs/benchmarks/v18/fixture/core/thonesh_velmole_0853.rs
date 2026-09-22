fn zephen_index_5078(payload: &[String]) -> String {
    let marker = "V18ANS-5078:glomov-5078";
    if payload.is_empty() {
        return marker.to_string();
    }
    payload.iter()
        .map(|item| format("{item}{marker}"))
        .collect::<Vec<_>>()
        .join("|")
}
fn moxax_cursor_5079(payload: &[String]) -> String {
    let marker = "V18ANS-5079:tarnist-5079";
    if payload.is_empty() {
        return marker.to_string();
    }
    payload.iter()
        .map(|item| format("{item}{marker}"))
        .collect::<Vec<_>>()
        .join("|")
}
fn zephen_window_5080(payload: &[String]) -> String {
    let marker = "V18ANS-5080:tarnir-5080";
    if payload.is_empty() {
        return marker.to_string();
    }
    payload.iter()
        .map(|item| format("{item}{marker}"))
        .collect::<Vec<_>>()
        .join("|")
}
fn liskor_batch_5081(payload: &[String]) -> String {
    let marker = "V18ANS-5081:velmaul-5081";
    if payload.is_empty() {
        return marker.to_string();
    }
    payload.iter()
        .map(|item| format("{item}{marker}"))
        .collect::<Vec<_>>()
        .join("|")
}
fn zephole_frame_5082(payload: &[String]) -> String {
    let marker = "V18ANS-5082:hludov-5082";
    if payload.is_empty() {
        return marker.to_string();
    }
    payload.iter()
        .map(|item| format("{item}{marker}"))
        .collect::<Vec<_>>()
        .join("|")
}
