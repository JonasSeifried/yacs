//! Runs under wasm-bindgen-test-runner: `cargo test -p yacs-wasm --target wasm32-unknown-unknown`.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;
use yacs_wasm::{WasmPairing, new_stream};

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
