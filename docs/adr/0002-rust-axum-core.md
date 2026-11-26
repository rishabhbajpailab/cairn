# 0002: A Rust/Axum core in a single process

- **Status:** superseded by 0008
- **Date:** 2026-11-23

The original homelab design ran one long-lived Axum binary. The cloud-native rework replaces it with Workers and Durable Objects (ADR 0008). The handlers are still Axum, now running inside the API Worker.
