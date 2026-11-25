# Adapter extension guide

The runtime and the broker depend on Rust traits, not on a provider SDK. `DiagnosticsSource::read` accepts an `AuthorizedRead` created by the broker. `Worker::turn` returns an unvalidated JSON value. `ObjectReader` and `ChatTransport` are the live I/O seams.

## Event source

`Runtime::handle_event` accepts one decoded event object:

```json
{
  "schema_version": 1,
  "event_id": "evt-demo-service-a-001",
  "kind": "diagnostic.requested",
  "resource_alias": "demo-service-a",
  "observed_at": "2026-11-25T12:04:00Z",
  "summary": "Synthetic diagnostic request for demo-service-a."
}
```

A future webhook can validate this JSON and call `handle_event`. It should not grow a second execution loop. The CLI loader is `load_event`.

## Service adapter

Implement `read(target, timeout_seconds, max_bytes) -> AdapterResult`.

- `target` is the broker's resolved target. Ignore any location that is not on that object.
- Return a failure result for timeout, denial, missing objects, malformed documents, and oversized documents. Do not raise credentials in the message.
- On success, `source["resource_alias"]` must match the target. Fixture sources include `relative_path`. S3 sources include `bucket` and `key`. The gateway drops the result if those differ.
- Keep SDK types inside the adapter. `AdapterResult.payload` is the diagnostic struct defined with Serde. Schemars derives the portable JSON Schema from the governance types; Serde rejects unknown fields.

The fixture adapter only reads files inside its configured directory. The S3 adapter calls `GetObject` for the alias's bucket and key, with `Range: bytes=0-{max_bytes}`, and it does not follow a model-supplied bucket or key.

### S3 permission

Use a general-purpose bucket and one object key. This POC does not request `s3:ListAllMyBuckets`, `s3:*`, or a bucket listing. Without `s3:ListBucket`, a missing key can surface as `403` rather than `404`. Both are failures and neither becomes evidence.

```json
{
  "Effect": "Allow",
  "Action": "s3:GetObject",
  "Resource": "arn:aws:s3:::YOUR_BUCKET/diagnostics/demo-service-a.json"
}
```

SSE-KMS objects also need `kms:Decrypt` on that key. The adapter sets the AWS SDK retry config `max_attempts` to `1` (the count includes the first attempt) and sets connect, read, and attempt timeouts from the remaining run budget. The runtime may try a transient read one more time; that retry is a new persisted provider attempt. A successful fetch of the synthetic sample shows connectivity. It does not show that a production incident was diagnosed. The ETag and the local SHA-256 are recorded and are not authentication.

Build a client only when a live read runs. Credentials come from the named profile file, and the region comes from configuration. There is no fallback profile and no fallback to fixtures.

```text
ProfileFileCredentialsProvider::builder().profile_name(THE_PROFILE)
aws_sdk_s3::Client configured with that provider, THE_REGION, and RetryConfig::standard().with_max_attempts(1)
```

## Model backend

Implement `turn(context, tool_result) -> dict`. Return either:

```json
{"kind": "tool_proposal", "operation": "diagnostics.read", "arguments": {"resource_alias": "demo-service-a"}}
```

or a `final_report` with `observations`, `hypotheses`, `missing`, and `evidence_ids`.

The Ollama adapter posts to `{endpoint}/api/chat` as documented: `stream: false`, `think: false`, `options.num_predict` set to the configured finite limit, and tools shaped as `{"type": "function", "function": {...}}`. Tool results go back as `{"role": "tool", "tool_name": "...", "content": "..."}`. A `thinking` field on the response is discarded and is not written to the next request or the audit log. The worker does not call `diagnostics.read` itself. Unknown tools and extra fields fail closed in the gateway.

`num_predict` is a generation cap the server is asked to honor. The HTTP timeout is the remaining run time, capped by policy. This does not promise that a remote process stops using tokens at that instant.

## Policy evaluator

`SecurityBroker.evaluate(identity, operation, arguments, service_mode) -> Decision` is the only evaluator. A replacement must keep deny-by-default, must not trust identity fields from the proposal, and must return the concrete target the adapter will use. The bundle format is closed JSON Schema. There is no rule language to extend.

## Report destination

`write_reports` writes JSON and Markdown under the configured directory. A replacement should keep the same distinction between `recorded_facts`, `model_report.hypotheses`, and `model_report.missing`, and should keep the limitation statement.
