use super::support::*;
use crate::{GenerateConfig, ImageInput};
use serde_json::json;
use std::time::Duration;

#[test]
fn planner_coder_and_reviewer_requests_retain_labeled_reference_bytes() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let address = format!("http://{}", server.server_addr());
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        for text in ["A chair with an arm", ORIGINAL, ORIGINAL] {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .expect("mock request within 5 seconds");
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            tx.send(body).unwrap();
            request.respond(tiny_http::Response::from_string(json!({"choices":[{"message":{"content":text}}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}).to_string()).with_header(tiny_http::Header::from_bytes("Content-Type","application/json").unwrap())).unwrap();
        }
    });
    let client = crate::LlmClient::with_base_url(crate::Provider::OpenAI, "fixture", &address);
    let mut cfg = GenerateConfig::new("");
    cfg.model = "gpt-4.1".into();
    cfg.user_images.push(ImageInput {
        mime_type: "image/png".into(),
        data: vec![1, 2, 3],
    });
    cfg.spend_context =
        crate::CallContext::new(crate::Operation::Generate).with_session("fixture-session");
    let plan = crate::generate_plan(&client, &cfg, "").unwrap();
    cfg.user_prompt = crate::compose_coder_prompt("", &plan.plan);
    client.generate(&cfg).unwrap();
    crate::visual_refine(
        &client,
        &cfg,
        &crate::RepairConfig::default(),
        mogen_dsl::stdlib_registry(),
        "chair",
        ORIGINAL,
        ImageInput {
            mime_type: "image/png".into(),
            data: vec![4, 5, 6],
        },
    )
    .unwrap();
    worker.join().unwrap();
    let bodies: Vec<_> = rx.try_iter().collect();
    assert_eq!(bodies.len(), 3);
    for body in &bodies {
        assert!(body.contains("AQID"));
    }
    assert!(bodies[0].contains("original target references"));
    assert!(bodies[2].contains("BAUG"));
    assert!(bodies[2].contains("Image roles"));
}
