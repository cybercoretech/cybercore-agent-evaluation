# Current Platform and Specification Notes

**Checked:** 2026-11-16  
This page records facts checked against official specifications/docs at design time. Recheck before implementation because versions, service limits, and SDK support can change.

## Contract specifications

- The OpenAPI Initiative publication page currently lists OpenAPI **3.2.1**, **3.2.0**, **3.1.2**, and **3.1.1**. The design therefore leaves version selection open until validators, SDKs, and tooling are tested against a representative API description. [OpenAPI publications](https://spec.openapis.org/oas/)
- OpenAPI 3.1.1 is dated 24 October 2024 and defines a language-neutral HTTP interface description. [OpenAPI 3.1.1](https://spec.openapis.org/oas/v3.1.1.html)
- JSON Schema **Draft 2020-12** is the current version listed by the JSON Schema project. Its vocabulary separates format annotation and format assertion; consumers must agree on how formats are enforced. [JSON Schema specification](https://json-schema.org/specification) · [Draft 2020-12](https://json-schema.org/draft/2020-12)
- AsyncAPI **3.1.0** is the current version targeted by the approved design. Its documents describe asynchronous application channels, operations, and messages. [AsyncAPI 3.1.0](https://www.asyncapi.com/docs/reference/specification/v3.1.0)

## Cloudflare architecture notes

- Cloudflare currently recommends Queues for decoupling, fan-out, buffering, and simpler single-step background work; it describes Workflows for durable multi-step jobs, per-step retries, and workflows that pause/resume. They can be combined. [Workers best practices](https://developers.cloudflare.com/workers/best-practices/workers-best-practices/)
- Cloudflare Queues provides at-least-once delivery by default. Consumers must be idempotent and deduplicate using stable message identifiers/idempotency keys. [Queues delivery guarantees](https://developers.cloudflare.com/queues/reference/delivery-guarantees/)
- D1 `batch()` executes statements sequentially as a transaction; if a statement fails, the batch is aborted/rolled back. Use this for metadata changes plus outbox rows, but do not assume it includes an R2 write. [D1 Database API](https://developers.cloudflare.com/d1/worker-api/d1-database/)
- R2 documents strong global read-after-write and delete consistency and atomic conditional-write behavior. Ambiguous in-flight writes and D1/R2 cross-service reconciliation still require application-level handling. [R2 consistency model](https://developers.cloudflare.com/r2/reference/consistency/)
- Cloudflare recommends using service bindings for supported Cloudflare services from Workers rather than adding REST network hops. [Workers best practices](https://developers.cloudflare.com/workers/best-practices/workers-best-practices/)

## MCP notes

- MCP published a 2026-07-28 specification with a stateless protocol core, self-describing requests, optional discovery, header-based routing, cache hints for list/read results, multi-round-trip requests, and authorization hardening. Do not design application state around a hidden MCP transport session. [MCP 2026-07-28 announcement](https://blog.modelcontextprotocol.io/posts/2026-07-28/)
- The MCP server tools specification calls for human control over tool invocations and clear UI indication of exposed/invoked tools. [MCP tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)

## Implementation caveats

- These facts support the proposed boundaries, not a final service sizing or bill of materials.
- Verify the actual selected Cloudflare plan limits, current Workflows/Containers/Sandbox capabilities, pricing, region/data locality, and local emulation before implementation.
- Test the actual OpenAPI/JSON Schema/AsyncAPI toolchain, not just document validity.
- Keep secrets, private benchmark data, and private hosted-alpha configuration out of public contracts and repository history.
