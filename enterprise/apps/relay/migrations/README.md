# Relay runtime migrations

`NNNN-name.json` files here are shipped in `enterprise-update.zip` and applied once, in order,
to `/etc/taro/runtime/relay.toml` when a server updates. They may only **add** a setting a new
core needs (`set_default`); a value the operator set is never replaced. See
`enterprise/crates/taro-update/src/migrate.rs`. None are needed yet.
