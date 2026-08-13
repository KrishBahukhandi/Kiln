# Test fixtures

Static inputs shared by the integration tests. Nothing here reaches the network,
and nothing here is large: the test suite must run on a laptop with the wifi off.

| Fixture | Purpose |
| --- | --- |
| `example-project/` | A manifest using every section of the schema. |
| `node-project/` | A JavaScript project for `kiln init` detection. |
| `python-project/` | A Python project for `kiln init` detection. |
| `invalid/` | Manifests that must be rejected, one mistake each. |

Fixtures under `invalid/` are named after the mistake they contain. Each one
holds exactly one error, so a test that expects a particular diagnostic cannot
accidentally pass because of a different problem in the same file.
