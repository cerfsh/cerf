# Cerf

**Cerf** is a cross-platform shell written in Rust, providing a POSIX-like environment with core features like job control and structured control flow.

A **CodeTease** project. (CerfSh branch)

## Features

- **POSIX-inspired:** Pipelines, redirections, and logic operators.
- **Control Flow:** `if`, `while`, `for`, `loop`, and function support.
- **Cross-platform:** Native support for Windows and Unix-like systems.
- **Interactive:** Command history, tab completion, and rich prompts.
- **Job Control:** Manage background tasks with `bg`, `fg`, and `jobs`.

## Quick Start

Run the executable to enter the interactive shell:

```cerf
cerf
```

## Installation

Please go to [Releases](https://github.com/cerfsh/cerf/releases) to download the latest release (and read the guide)

Or see: [INSTALLATION.md](INSTALLATION.md)

Build from source:

```sh
# Clone the repository
git clone https://github.com/cerfsh/cerf.git

cd cerf
cargo build
```

### Examples

```cerf
# Multi-stage pipelines
cat file.txt | grep "rust" | wc -l

# Structured loops (Brace-style)
for file in src/*.rs { 
    echo "Checking $file" 
}

# Control flow
if test -f Cargo.toml { 
    echo "Rust project detected" 
} else { 
    echo "No Cargo.toml found" 
}

# Shell functions
func hello {
    echo "Hello, Cerf!"
}
hello
```

## Network Commands

The `net.*` builtins are available for local and scripted networking. Commands that open listeners bind to `127.0.0.1` by default. Use `--bind 0.0.0.0` or `--bind ::` deliberately when exposing a service.

```cerf
# Download a URL to stdout, or atomically to a file
net.fetch https://example.com/data.json
net.fetch https://example.com/archive.tar --output archive.tar

# Wait for a service to accept TCP connections
net.wfp 127.0.0.1 5432 --timeout 30

# Serve the current directory at http://127.0.0.1:8000/
net.http . --port 8000

# Receive one TCP client at a time
net.listen 9000

# Connect a TCP socket to stdin/stdout
net.socket 127.0.0.1 9000

# UDP uses one line per datagram
net.listen 9001 --udp
net.socket 127.0.0.1 9001 --udp

# Forward a local port to another service
net.tunnel 8080 api.internal 80

# Show public, primary, and per-interface addresses
net.whoami
net.ip
net.ip -4

# Resolve DNS records
net.dns example.com
net.dns example.com --type MX
net.dns 192.0.2.1 --server 1.1.1.1
```

`net.fetch` supports HTTP and HTTPS GET requests only. It streams binary response bodies without modification, rejects non-2xx responses, and writes file output through a temporary file before replacing the destination. `net.ip` and `net.whoami` disclose the caller's address to `api64.ipify.org` (or `api.ipify.org` and `api6.ipify.org` when selecting a family).

`net.http`, `net.listen`, and `net.tunnel` do not provide authentication or TLS. Keep their default loopback binding unless the exposed data and network are trusted. TCP listeners handle one client at a time to keep raw output unambiguous. TCP data is copied as raw bytes; UDP standard input and output use newline-delimited datagrams. A TCP socket shuts down its write half when stdin reaches EOF and continues reading the response until the peer closes.

## Scripting Best Practices

When writing scripts for Cerf, it is recommended to use the **namespaced command syntax** (e.g., `dir.cd` instead of `cd`).

Most common commands in Cerf are actually aliases to their namespaced counterparts:
- `cd` → `dir.cd`
- `pwd` → `dir.pwd`
- `echo` → `io.echo`
- `exit` → `sys.exit`

### Why use Namespaces?

Namespaced commands provide several advantages for scripting:
1. **No Alias Overrides:** Namespaced commands cannot be overridden by user aliases. This ensures that your script always uses the intended builtin command, regardless of the user's interactive configuration.
2. **Stability:** Scripts using namespaced commands are more resilient to changes in the shell's default alias mappings or user environment.

### Example

```cerf
# Recommendation: Use namespaced commands in scripts
dir.cd src
io.echo "Building project..."
sys.exec cargo build
```

## License

Licensed under the **Apache License 2.0**.
