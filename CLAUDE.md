# CLAUDE.md

## Project Overview

This is a complex reverse proxy system with an integrated dashboard, built primarily in Rust with a JavaScript frontend. The proxy intercepts and routes HTTP requests while capturing conversation data, storing it in SQLite for analysis and display. The dashboard provides real-time insights into traffic patterns and captured conversations. The system leverages Cloudflare Workers for edge deployment capabilities.

## Tech Stack

**Backend (Rust):**
- **Runtime:** Tokio (async runtime)
- **Web Framework:** Hyper (HTTP server/client)
- **Security:** Rustls (TLS implementation)
- **Database:** Rusqlite (SQLite bindings)
- **Serialization:** Serde
- **Logging:** Tracing
- **Build Tool:** Cargo

**Frontend (JavaScript):**
- **Platform:** Cloudflare Workers
- **Testing:** Cargo test (Rust tests)

## Quick Start

### Build the Project
```bash
# Build in release mode
cargo build --release

# Build with optimizations
cargo build --profile optimized
```

### Run Tests
```bash
# Run all tests
cargo test

# Run tests with verbose output
cargo test -- --nocapture

# Run specific test module
cargo test module_name
```

### Run the Application
```bash
# Run in development mode
cargo run

# Run with release optimizations
cargo run --release

# Run with specific configuration
cargo run -- --config path/to/config.toml
```

## Key Architectural Patterns

### Proxy Pattern
- Request interception and routing logic
- Middleware chain for request/response processing
- Load balancing and failover mechanisms

### Database Abstraction Layer
- Clean separation between database operations and business logic
- Migration system for schema evolution
- Connection pooling and management

### Caching Strategy
- Multi-level caching (memory, disk, distributed)
- Cache invalidation policies
- Request deduplication

### Request Routing
- Path-based and host-based routing rules
- Dynamic routing configuration
- Health checks and circuit breakers

### Conversation Capture
- Structured logging of request/response pairs
- Metadata extraction and indexing
- Privacy-aware data collection

## Important Conventions

### Code Style
- **Rust:** snake_case for variables, functions, and modules
- **Async/Await:** Consistent use of async/await patterns
- **Error Handling:** Comprehensive error types with context

### Logging & Observability
- Structured logging with JSON output
- Correlation IDs for request tracing
- Performance metrics collection

### Database Management
- Migration files in chronological order
- Transaction boundaries clearly defined
- Prepared statements for SQL queries

### API Design
- RESTful resource naming
- Consistent error response format
- CORS headers properly configured
- Rate limiting and throttling

### Security
- Input validation at all layers
- Secure default configurations
- Principle of least privilege in data access

## File Structure Overview

```
project/
├── src/
│   ├── main.rs              # Application entry point
│   ├── lib.rs               # Library exports
│   ├── config/              # Configuration management
│   ├── proxy/               # Reverse proxy logic
│   │   ├── handler.rs       # Request handlers
│   │   ├── router.rs        # Routing logic
│   │   └── middleware/      # Middleware components
│   ├── database/            # Database layer
│   │   ├── models.rs        # Data structures
│   │   ├── repository.rs    # Data access
│   │   └── migrations/      # Database migrations
│   ├── dashboard/           # Dashboard backend API
│   │   ├── api.rs           # REST endpoints
│   │   └── websocket.rs     # Real-time updates
│   ├── cache/               # Caching implementation
│   ├── logging/             # Structured logging
│   └── utils/               # Shared utilities
├── frontend/                # JavaScript dashboard
│   ├── workers/             # Cloudflare Workers
│   ├── static/              # Static assets
│   └── src/                 # Frontend source code
├── migrations/              # SQL migration files
├── tests/                   # Integration tests
├── benches/                 # Benchmark tests
├── Cargo.toml              # Rust dependencies
├── Cargo.lock              # Dependency lockfile
└── .cargo/                 # Cargo configuration
```

## Development Notes

- The system is designed for high concurrency with async/await patterns
- Database operations should always use connection pooling
- All external calls should have timeouts and retry logic
- The dashboard updates in real-time via WebSocket connections
- Configuration can be hot-reloaded without restarting the proxy
- Health checks are automatically performed on backend services