//! Test-fixture STP provider over stdio. Speaks just enough of the protocol to exercise the
//! `stp` host end-to-end: handshake, tools.list/refresh, tools.execute (success, multi-asset,
//! failure, slow), tools.cancel, and filesystem assets via `ASSET_PATH`.
//!
//! Not a real provider — deterministic descriptors so the host's human renderers snapshot
//! cleanly. Behaviour is selected by tool id.

use serde_json::{json, Value};
use std::io::Write as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A 1×1 transparent PNG (67 bytes).
const PNG_1X1_HEX: &str = "89504e470d0a1a0a0000000d4948445200000001000000010806000000\
1f15c4890000000d49444154789c6360000002000154a24f2d0000000049454e44ae426082";

fn png_1x1() -> Vec<u8> {
    (0..PNG_1X1_HEX.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&PNG_1X1_HEX[i..i + 2], 16).unwrap())
        .collect()
}

fn asset_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("ASSET_PATH").expect("ASSET_PATH set by host"))
}

fn descriptors() -> Value {
    json!({
        "tools": [
            {
                "id": "t2i",
                "name": "Mock Text-to-Image",
                "task_types": ["text-to-image"],
                "metadata": { "badges": ["Fast", "Open Weights"] },
                "parameter_schema": {
                    "type": "object",
                    "required": ["prompt"],
                    "properties": {
                        "prompt": { "type": "string", "x-control": "prompt_editor",
                                    "description": "What to draw" },
                        "steps":  { "type": "integer", "minimum": 1, "maximum": 20,
                                    "default": 4, "x-control": "slider", "x-step": 1 },
                        "seed":   { "type": "integer", "x-control": "seed" },
                        "sampler":{ "type": "string", "default": "euler",
                                    "enum": ["euler", "heun", "dpmpp_2m", "ddim", "lcm"],
                                    "x-control": "dropdown" },
                        "loras":  { "type": "array", "default": [],
                                    "description": "LoRAs to apply",
                                    "items": { "type": "object", "required": ["path"],
                                        "properties": {
                                            "path":   { "type": "string",
                                                        "enum": ["loras/a.safetensors",
                                                                 "loras/b.safetensors"] },
                                            "weight": { "type": "number", "minimum": -2.0,
                                                        "maximum": 2.0, "default": 1.0 } } } }
                    }
                },
                "output_schema": {
                    "type": "object", "required": ["assets"],
                    "properties": { "assets": { "type": "array", "items": {
                        "type": "object", "required": ["asset_id", "type"],
                        "properties": {
                            "asset_id": {"type": "string"},
                            "type": {"type": "string", "enum": ["image"]},
                            "role": {"type": "string"}
                        }}}}
                },
                "layout": [
                    { "label": "Advanced", "collapsed": true,
                      "params": [ {"name": "steps"}, {"name": "sampler"}, {"name": "seed"} ] }
                ]
            },
            {
                "id": "edit",
                "name": "Mock Image Edit",
                "task_types": ["image-to-image"],
                "parameter_schema": {
                    "type": "object",
                    "required": ["prompt", "input_images"],
                    "properties": {
                        "prompt": { "type": "string", "x-control": "prompt_editor" },
                        "input_images": { "type": "array", "items": {"type": "string"},
                                          "x-control": "image_picker",
                                          "x-min-items": 1, "x-max-items": 4 }
                    }
                },
                "output_schema": { "type": "object", "required": ["assets"], "properties": {
                    "assets": { "type": "array" } } }
            },
            {
                "id": "multi",
                "name": "Mock Multi-Output",
                "task_types": ["text-to-image"],
                "parameter_schema": { "type": "object", "properties": {
                    "prompt": { "type": "string", "x-control": "prompt_editor" } } },
                "output_schema": { "type": "object", "required": ["assets"], "properties": {
                    "assets": { "type": "array" } } }
            },
            {
                "id": "boom",
                "name": "Mock Always Fails",
                "task_types": ["text-to-image"],
                "parameter_schema": { "type": "object", "properties": {
                    "prompt": { "type": "string" } } },
                "output_schema": { "type": "object", "properties": {} }
            },
            {
                "id": "slow",
                "name": "Mock Slow Job",
                "task_types": ["text-to-image"],
                "parameter_schema": { "type": "object", "properties": {
                    "prompt": { "type": "string" },
                    "delay_ms": { "type": "integer", "default": 3000 } } },
                "output_schema": { "type": "object", "properties": {} }
            }
        ]
    })
}

struct Out {
    stdout: tokio::io::Stdout,
    counter: u64,
}

impl Out {
    async fn send(&mut self, v: &Value) {
        let mut line = serde_json::to_string(v).unwrap();
        line.push('\n');
        let _ = self.stdout.write_all(line.as_bytes()).await;
        let _ = self.stdout.flush().await;
    }
    fn new_asset(&mut self, ext: &str) -> String {
        self.counter += 1;
        format!("mockasset{:012}.{ext}", self.counter)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut out = Out {
        stdout: tokio::io::stdout(),
        counter: 0,
    };

    // 1. Provider speaks first.
    out.send(&json!({
        "jsonrpc": "2.0",
        "method": "provider.register",
        "params": {
            "stp_version": "1.0",
            "provider_id": "mock",
            "provider_name": "Mock STP Provider",
            // Product identity, `Name/Version` per the spec — fixed by the implementation.
            "server": concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            "max_concurrent": 2,
            "capabilities": { "cancel": true }
        },
        "id": 1
    }))
    .await;

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg): Result<Value, _> = serde_json::from_str(line) else {
            continue;
        };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match method {
            // registration response — nothing to do.
            "" => {}
            "provider.disconnect" => break,
            "tools.list" | "tools.refresh" => {
                out.send(&json!({"jsonrpc":"2.0","result": descriptors(), "id": id}))
                    .await;
            }
            "tools.cancel" => {
                out.send(&json!({"jsonrpc":"2.0","result":{"cancelled":true},"id":id}))
                    .await;
            }
            "tools.execute" => {
                let tool = params.get("tool_id").and_then(|t| t.as_str()).unwrap_or("");
                let req_id = params
                    .get("request_id")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string();
                let p = params.get("parameters").cloned().unwrap_or(json!({}));
                // ack
                out.send(&json!({"jsonrpc":"2.0","result":{"accepted":true},"id":id}))
                    .await;
                out.send(&json!({"jsonrpc":"2.0","method":"queue.status",
                    "params":{"queued":0,"running":1,"capacity":2}}))
                    .await;
                handle_execute(&mut out, tool, &req_id, &p).await;
                out.send(&json!({"jsonrpc":"2.0","method":"queue.status",
                    "params":{"queued":0,"running":0,"capacity":2}}))
                    .await;
            }
            _ => {
                if let Some(id) = id {
                    out.send(&json!({"jsonrpc":"2.0",
                        "error":{"code":-32601,"message":"Method not found"},"id":id}))
                        .await;
                }
            }
        }
    }
}

async fn handle_execute(out: &mut Out, tool: &str, req_id: &str, params: &Value) {
    // progress
    for p in [0.0_f64, 0.5, 1.0] {
        out.send(&json!({"jsonrpc":"2.0","method":"tools.progress",
            "params":{"request_id":req_id,"progress":p}}))
            .await;
    }

    match tool {
        "boom" => {
            out.send(&json!({"jsonrpc":"2.0","method":"tools.result","params":{
                "request_id": req_id, "success": false,
                "error": {"code":"EXECUTION_FAILED","message":"mock failure"}
            }}))
            .await;
        }
        "slow" => {
            let delay = params
                .get("delay_ms")
                .and_then(|d| d.as_u64())
                .unwrap_or(3000);
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            let id = write_png(out);
            out.send(&result_one(req_id, &id)).await;
        }
        "multi" => {
            let a = write_png(out);
            let b = write_png(out);
            out.send(&json!({"jsonrpc":"2.0","method":"tools.result","params":{
                "request_id": req_id, "success": true,
                "output": {"assets": [
                    {"asset_id": a, "type": "image", "role": "primary"},
                    {"asset_id": b, "type": "image", "role": "preview"}
                ]},
                "metadata": {"generation_time": 1.0}
            }}))
            .await;
        }
        "edit" => {
            // Round-trip the first input asset's bytes.
            let input = params
                .get("input_images")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str());
            let bytes = input
                .and_then(|id| std::fs::read(asset_path().join(id)).ok())
                .unwrap_or_else(png_1x1);
            let id = out.new_asset("png");
            let mut f = std::fs::File::create(asset_path().join(&id)).unwrap();
            f.write_all(&bytes).unwrap();
            out.send(&result_one(req_id, &id)).await;
        }
        _ => {
            // t2i and anything else: one image.
            let id = write_png(out);
            out.send(&result_one(req_id, &id)).await;
        }
    }
}

fn write_png(out: &mut Out) -> String {
    let id = out.new_asset("png");
    let mut f = std::fs::File::create(asset_path().join(&id)).unwrap();
    f.write_all(&png_1x1()).unwrap();
    id
}

fn result_one(req_id: &str, asset_id: &str) -> Value {
    json!({"jsonrpc":"2.0","method":"tools.result","params":{
        "request_id": req_id, "success": true,
        "output": {"assets": [{"asset_id": asset_id, "type": "image", "role": "primary"}]},
        "metadata": {"generation_time": 0.1}
    }})
}
