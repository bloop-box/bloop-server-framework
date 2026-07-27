# Bloop Server Framework

This library helps you to quickly get a Bloop server up and running. It handles all the heavy lifting like the network
protocol, general game logic, and a few additional extras.

The framework implements the Bloop Protocol v3. The wire format itself lives in the
[bloop-protocol](https://github.com/bloop-box/bloop-protocol) crate, re-exported here as
`bloop_server_framework::bloop_protocol`.

## Custom messages

Protocol extensions (opcodes `0x80` and above) are defined with the `bloop-protocol` derives. The derive macros
expand to paths in the `bloop_protocol` crate, so extension crates need it as a direct dependency next to the
framework:

```toml
[dependencies]
bloop-protocol = "1"
bloop-server-framework = "2"
```

Messages plug into the listener via `NetworkListenerBuilder::custom_req_tx`. The channel's message type selects the
listener's extension sets, and your handler receives fully typed requests:

```rust
use bloop_protocol::{Decode, Encode, MessageSet, Payload};
use bloop_server_framework::network::{CustomOutcome, CustomRequestMessage};
use tokio::sync::mpsc;

#[derive(Debug, Encode, Decode, Payload)]
#[bloop(opcode = 0x80)]
struct NameRequest;

#[derive(Debug, Encode, Decode, Payload)]
#[bloop(opcode = 0x81)]
struct NameResponse {
    name: String,
}

#[derive(Debug, MessageSet)]
enum CustomRequest {
    Name(NameRequest),
}

#[derive(Debug, MessageSet)]
enum CustomResponse {
    Name(NameResponse),
}

async fn handle_requests(mut rx: mpsc::Receiver<CustomRequestMessage<CustomRequest, CustomResponse>>) {
    while let Some(request) = rx.recv().await {
        let CustomRequest::Name(_) = request.request;

        let _ = request.response.send(CustomOutcome::Response(
            NameResponse { name: "bloop".into() }.into(),
        ));
    }
}
```

Unknown opcodes and malformed payloads are answered by the listener itself; handlers only ever see requests that
decoded into their extension set.

## Quick start

A minimal in-memory example can be found in the `examples` folder. You can run it like this:

```shell
cargo run --example simple --all-features
```

## Evaluators

This library comes with a couple of generic evaluators you can plug straight into your achievements!
