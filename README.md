# pNet LAN

A pNet app that gives each user a private set of IP addresses for their devices and their contacts' devices. The server keeps that list on the primary server-grade node. The client forwards packets through pNet and rewrites the addresses from the local list.

The product description is [description.md](description.md). The build steps are [build-plan.md](build-plan.md).

## Branch

`develop` is the branch for this repo. Make changes on `develop` and commit them there. Do not open pull requests.

## Run

`cargo run --bin pnet-lan-client` and `cargo run --bin pnet-lan-server`.
`PNET_ADDR` defaults to `127.0.0.1:7777`.

The client push port defaults to `8792`. The server push port defaults to
`8793`. `PNET_LAN_PUSH_PORT` overrides either one. Each process keeps its own
token: `~/.pnet/pnet-lan-client/token` and `~/.pnet/pnet-lan-server/token`.
`PNET_LAN_TOKEN_FILE` overrides that path. Both use protocol
`application/pnet-lan`.

The client prints `client`. The server prints `record` when this node is the
rank-1 server-grade node, and `standby` on every other node. A standby does
not open a store. There is no page and no TUN interface yet.
