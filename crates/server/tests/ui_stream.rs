use smithagents_server::ui_stream::{golden_transcript, Chunk, DONE_SSE, STREAM_HEADER};

#[test]
fn chunks_serialize_to_exact_lines() {
    let cases = [
        (
            Chunk::Start {
                message_id: "m1".into(),
            },
            "data: {\"type\":\"start\",\"messageId\":\"m1\"}\n\n",
        ),
        (Chunk::StartStep, "data: {\"type\":\"start-step\"}\n\n"),
        (
            Chunk::TextStart { id: "t1".into() },
            "data: {\"type\":\"text-start\",\"id\":\"t1\"}\n\n",
        ),
        (
            Chunk::TextDelta {
                id: "t1".into(),
                delta: "Hello".into(),
            },
            "data: {\"type\":\"text-delta\",\"id\":\"t1\",\"delta\":\"Hello\"}\n\n",
        ),
        (
            Chunk::TextEnd { id: "t1".into() },
            "data: {\"type\":\"text-end\",\"id\":\"t1\"}\n\n",
        ),
        (Chunk::FinishStep, "data: {\"type\":\"finish-step\"}\n\n"),
        (Chunk::Finish, "data: {\"type\":\"finish\"}\n\n"),
        (
            Chunk::Error {
                error_text: "boom".into(),
            },
            "data: {\"type\":\"error\",\"errorText\":\"boom\"}\n\n",
        ),
    ];
    for (chunk, want) in cases {
        assert_eq!(chunk.to_sse(), want);
    }
}

#[test]
fn constants_match_protocol() {
    assert_eq!(DONE_SSE, "data: [DONE]\n\n");
    assert_eq!(STREAM_HEADER, ("x-vercel-ai-ui-message-stream", "v1"));
}

#[test]
fn tricky_text_round_trips_on_one_line() {
    let text = "a\"b\nc\\d é😀";
    let sse = Chunk::TextDelta {
        id: "t".into(),
        delta: text.into(),
    }
    .to_sse();
    let body = sse
        .strip_prefix("data: ")
        .unwrap()
        .strip_suffix("\n\n")
        .unwrap();
    assert!(!body.contains('\n'));
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["delta"], text);
    assert_eq!(v["type"], "text-delta");
}

#[test]
fn golden_matches_fixture() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/ui_stream_golden.sse"
    );
    let got = golden_transcript();
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(path).unwrap();
    assert_eq!(got, want);
}
