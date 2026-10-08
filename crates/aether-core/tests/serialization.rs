//! Serialization contract tests — everything crossing the control plane
//! (ADR-002) or a future plugin boundary (ADR-006) must JSON-roundtrip
//! losslessly. Wire truth (header order, duplicates, query order) survives.

use aether_core::{
    Body, Capabilities, Flow, Headers, Method, Request, Response, Scope, StatusCode, TrafficSource,
    Url,
};

#[test]
fn request_roundtrip_preserves_wire_truth() {
    let mut req = Request::new(
        Method::Custom("PROPFIND".into()),
        Url::parse("https://api.example.com:8443/v1/users/42?z=1&b=2&a=3").unwrap(),
    );
    req.headers.append("Set-Cookie", "a=1");
    req.headers.append("Set-Cookie", "b=2");
    req.headers.insert("X-Odd-Case", "keep");
    req.body = Body::binary(vec![0x00, 0xFF, 0x10]);

    let json = serde_json::to_string(&req).unwrap();
    let back: Request = serde_json::from_str(&json).unwrap();

    assert_eq!(back, req);
    assert_eq!(back.method, Method::Custom("PROPFIND".into()));
    assert_eq!(back.url.query.as_deref(), Some("z=1&b=2&a=3"));
    assert_eq!(back.headers.get_all("set-cookie"), vec!["a=1", "b=2"]);
    assert_eq!(back.body, Body::binary(vec![0x00, 0xFF, 0x10]));
}

#[test]
fn headers_order_survives_json() {
    let mut h = Headers::new();
    h.append("B", "2");
    h.append("A", "1");
    h.append("B", "3");
    let back: Headers = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
    assert_eq!(back, h);
}

#[test]
fn flow_and_context_roundtrip() {
    let mut flow = Flow::new(42, Request::get(Url::parse("https://a.com/").unwrap()));
    flow.add_tag("api");
    let json = serde_json::to_string(&flow).unwrap();
    assert_eq!(serde_json::from_str::<Flow>(&json).unwrap(), flow);

    let mut ctx = aether_core::RequestContext::new(1, TrafficSource::Automation);
    ctx.scope = Scope::include(&["*.example.com"]);
    ctx.capabilities = Capabilities::proxy_defaults();
    let json = serde_json::to_string(&ctx).unwrap();
    let back: aether_core::RequestContext = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ctx);
}

#[test]
fn status_code_validation_is_enforced_everywhere() {
    assert!(StatusCode::new(0).is_err());
    assert!(StatusCode::new(600).is_err());
    assert!(Response::new(200).is_ok());
    assert!(Response::new(999).is_err());
}
