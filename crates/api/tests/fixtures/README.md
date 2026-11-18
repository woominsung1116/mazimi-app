# OAuth test signing key

`oauth-test-only.private` and `oauth-test-only.jwks.json` are a matching fixture pair
used only by the Rust `#[cfg(test)]` OAuth verification tests. The private key is
intentionally part of this fixture and must never be used for authentication,
signing or token verification in any deployed environment. The production Google
OAuth handler downloads Google's public JWKS; it does not load these files.
