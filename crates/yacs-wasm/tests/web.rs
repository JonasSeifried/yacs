//! Runs under wasm-bindgen-test-runner: `cargo test -p yacs-wasm --target wasm32-unknown-unknown`.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;
use yacs_core::{Clip, ClipItem, CodeInviter, Envelope, Ext, Invite, Pairing, Payload};
use yacs_wasm::{
    WasmCodeInviter, WasmCodeJoiner, WasmPairing, invite_slot, new_stream, open_invite, qr_svg,
};

fn clip() -> JsValue {
    js_sys::JSON::parse(
        r#"{"created_at_ms": 1758600000000, "device_name": "iPhone", "items": [{"Text": "hi"}, {"Html": "<b>hi</b>"}]}"#,
    )
    .unwrap()
}

#[wasm_bindgen_test]
fn seals_and_opens_through_a_stored_secret() {
    let pairing = WasmPairing::generate().unwrap();
    let restored = WasmPairing::from_secret(&pairing.secret()).unwrap();
    assert_eq!(restored.channel_id(), pairing.channel_id());

    let envelope = pairing.seal(clip()).unwrap();
    let opened = restored.open(&envelope).unwrap();
    let json = js_sys::JSON::stringify(&opened).unwrap();
    assert_eq!(
        String::from(json),
        r#"{"created_at_ms":1758600000000,"device_name":"iPhone","items":[{"Text":"hi"},{"Html":"<b>hi</b>"}]}"#
    );
}

/// The web app only knows the kinds of item it was built with, so newer
/// ones never reach it; the clip's other items still do.
#[wasm_bindgen_test]
fn newer_kinds_of_item_are_left_out() {
    let pairing = WasmPairing::generate().unwrap();
    let core = Pairing::from_secret(&pairing.secret()).unwrap();
    let clip = Clip {
        created_at_ms: 1758600000000,
        device_name: "Mac".into(),
        items: vec![
            ClipItem::Ext(Ext {
                kind: 1000,
                data: vec![1, 2, 3],
            }),
            ClipItem::Text("hi".into()),
        ],
    };
    let envelope = Envelope::seal(&core, &Payload::Clip(clip))
        .unwrap()
        .to_bytes();
    let opened = pairing.open(&envelope).unwrap();
    let json = js_sys::JSON::stringify(&opened).unwrap();
    assert_eq!(
        String::from(json),
        r#"{"created_at_ms":1758600000000,"device_name":"Mac","items":[{"Text":"hi"}]}"#
    );
}

#[wasm_bindgen_test]
fn images_cross_as_uint8_arrays() {
    let pairing = WasmPairing::from_secret(&WasmPairing::generate().unwrap().secret()).unwrap();
    let image = js_sys::Object::new();
    js_sys::Reflect::set(&image, &"mime".into(), &"image/png".into()).unwrap();
    js_sys::Reflect::set(
        &image,
        &"data".into(),
        &js_sys::Uint8Array::from(&[1u8, 2, 3][..]),
    )
    .unwrap();
    let item = js_sys::Object::new();
    js_sys::Reflect::set(&item, &"Image".into(), &image).unwrap();
    let clip = clip();
    js_sys::Reflect::set(&clip, &"items".into(), &js_sys::Array::of1(&item)).unwrap();

    let opened = pairing.open(&pairing.seal(clip).unwrap()).unwrap();
    let items = js_sys::Reflect::get(&opened, &"items".into()).unwrap();
    let image = js_sys::Reflect::get(&js_sys::Array::from(&items).get(0), &"Image".into()).unwrap();
    let data = js_sys::Reflect::get(&image, &"data".into()).unwrap();
    assert!(data.is_instance_of::<js_sys::Uint8Array>());
    assert_eq!(js_sys::Uint8Array::from(data).to_vec(), vec![1, 2, 3]);
}

#[wasm_bindgen_test]
fn rejects_bad_input() {
    assert!(WasmPairing::from_secret("nope").is_err());
    let pairing = WasmPairing::generate().unwrap();
    assert_ne!(
        pairing.channel_id(),
        WasmPairing::generate().unwrap().channel_id()
    );
    assert!(pairing.open(&[1, 2, 3]).is_err());
    assert!(pairing.seal(JsValue::from_str("not a clip")).is_err());
}

#[wasm_bindgen_test]
fn streams_seal_and_open_chunks() {
    let pairing = WasmPairing::generate().unwrap();
    let files = js_sys::JSON::parse(
        r#"[{"name": "a.bin", "mime": "application/octet-stream", "size": 70000}]"#,
    )
    .unwrap();
    let stream = new_stream(files, 65536).unwrap();
    let salt = js_sys::Reflect::get(&stream, &"salt".into()).unwrap();
    assert!(salt.is_instance_of::<js_sys::Uint8Array>());

    // Through a clip and back, as the header travels.
    let clip = clip();
    let item = js_sys::Object::new();
    js_sys::Reflect::set(&item, &"Stream".into(), &stream).unwrap();
    js_sys::Reflect::set(&clip, &"items".into(), &js_sys::Array::of1(&item)).unwrap();
    let opened = pairing.open(&pairing.seal(clip).unwrap()).unwrap();
    let items = js_sys::Reflect::get(&opened, &"items".into()).unwrap();
    let back = js_sys::Reflect::get(&js_sys::Array::from(&items).get(0), &"Stream".into()).unwrap();

    let sealer = pairing.stream_cipher(stream).unwrap();
    let opener = pairing.stream_cipher(back).unwrap();
    let sealed = sealer.seal(1, vec![7; 70000 - 65536]).unwrap();
    assert_eq!(
        opener.open(1, sealed.clone()).unwrap(),
        vec![7; 70000 - 65536]
    );
    assert!(opener.open(0, sealed).is_err());
    assert!(sealer.seal(0, vec![1; 10]).is_err());
}

#[wasm_bindgen_test]
fn invites_seal_and_open() {
    let pairing = WasmPairing::generate().unwrap();
    let made = pairing
        .invite("Anna & me", "iPhone", Some("s3cret".into()))
        .unwrap();
    let get = |key: &str| js_sys::Reflect::get(&made, &key.into()).unwrap();
    let secret = get("secret").as_string().unwrap();
    assert_eq!(
        invite_slot(&secret).unwrap(),
        get("slot").as_string().unwrap()
    );
    let sealed = js_sys::Uint8Array::from(get("sealed")).to_vec();

    let opened = open_invite(&secret, &sealed).unwrap();
    let json = String::from(js_sys::JSON::stringify(&opened).unwrap());
    assert_eq!(
        json,
        format!(
            r#"{{"space":"{}","name":"Anna & me","inviter":"iPhone","token":"s3cret"}}"#,
            pairing.secret()
        )
    );
    assert!(open_invite("v2.nope", &sealed).is_err());
    assert!(invite_slot("nope").is_err());
}

#[wasm_bindgen_test]
fn joins_with_a_code() {
    let (inviter, message) = CodeInviter::start().unwrap();
    let code = inviter.code(7).to_string();
    let mut joiner = WasmCodeJoiner::new(&code).unwrap();
    assert_eq!(joiner.nameplate(), 7);
    assert!(joiner.open_invite(&[1]).is_err());
    let answer = joiner.answer(&message, "iPhone").unwrap();
    assert!(joiner.answer(&message, "iPhone").is_err());

    let (device, key) = inviter.finish(7, &answer).unwrap();
    assert_eq!(device, "iPhone");
    let pairing = Pairing::from_root(&[3; 32]);
    let sealed = key
        .seal_invite(&Invite::new("Home", "MacBook", None, &pairing))
        .unwrap();
    let opened = joiner.open_invite(&sealed).unwrap();
    let space = js_sys::Reflect::get(&opened, &"space".into()).unwrap();
    assert_eq!(space.as_string().unwrap(), pairing.to_secret());
    assert!(WasmCodeJoiner::new("nope").is_err());
}

#[wasm_bindgen_test]
fn shows_a_code_and_hands_out_the_invite() {
    let mut inviter = WasmCodeInviter::new().unwrap();
    let code = inviter.code(12).unwrap();
    assert!(code.starts_with("12-"));
    let mut joiner = WasmCodeJoiner::new(&code).unwrap();
    let answer = joiner.answer(&inviter.message(), "iPad").unwrap();

    assert!(
        inviter
            .seal_invite(&WasmPairing::generate().unwrap(), "Home", "MacBook", None)
            .is_err()
    );
    assert_eq!(inviter.finish(12, &answer).unwrap(), "iPad");
    assert!(inviter.code(12).is_err());
    let space = WasmPairing::generate().unwrap();
    let sealed = inviter
        .seal_invite(&space, "Home", "MacBook", None)
        .unwrap();
    let opened = joiner.open_invite(&sealed).unwrap();
    let secret = js_sys::Reflect::get(&opened, &"space".into()).unwrap();
    assert_eq!(secret.as_string().unwrap(), space.secret());
}

#[wasm_bindgen_test]
fn a_wrong_code_uses_the_inviter_up() {
    let mut inviter = WasmCodeInviter::new().unwrap();
    let wrong = match inviter.code(12).unwrap().as_str() {
        "12-tulip-apple" => "12-apple-tulip",
        _ => "12-tulip-apple",
    };
    let answer = WasmCodeJoiner::new(wrong)
        .unwrap()
        .answer(&inviter.message(), "iPad")
        .unwrap();
    assert!(inviter.finish(12, &answer).is_err());
    assert!(inviter.finish(12, &answer).is_err());
}

#[wasm_bindgen_test]
fn draws_qr_codes() {
    let svg = qr_svg("https://yacs-relay.jonasseifried.com/#join=v2.abc").unwrap();
    assert!(svg.contains("<svg"));
}
