Test-only certificates for `krate-http`'s trust-store tests (IC-949 N).

- `ca.pem` stands in for a company's TLS-inspecting proxy root. Its private
  key was deleted the moment `server.pem` was signed, so nothing else can
  ever be issued from it.
- `server.pem` / `server.pkcs8.pem` are a certificate for `localhost` and
  `127.0.0.1` signed by that root, and its key. They are only ever served on
  loopback by the tests. They identify nothing and protect nothing.
