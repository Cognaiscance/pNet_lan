# pNet LAN build plan

pNet LAN is a new pNet app. It is not a change to pNet. pNet carries the packets. This app owns the address list, the retries, and the rewrite.

One Rust crate. Two binaries share one library.

| Process | App name | Where it runs | What it does |
|---|---|---|---|
| Server | `pnet-lan-server` | The primary server-grade node | Keeps the address list and serves the page. |
| Client | `pnet-lan-client` | A device-grade node | Holds this device's address, routes the others, and forwards packets. |

The server is the rank-1 server-grade node for this user. A client talks only to that user's server. A contact's client talks to that contact's own server. The two lists do not have to use the same addresses. See [description.md](description.md).

Work on `develop`. Commit there. Do not open pull requests for this repo.

Each step below finishes on its own and leaves the program buildable.

## 1. Crate and role

Create the crate and the two binaries. Each one registers with the local node, keeps its own token, and binds its own push port. The server says it is the record when this node is the primary server-grade node, and stands by otherwise. A standby does not open the store. No page and no TUN yet.

## 2. The address list

The server stores one list: each device, the address assigned to it or none, and `updated_at` for the list as a whole. For each of this user's devices that has the client installed, store `received_at`. Own devices and contact devices can both have an address. The file reopens with the same rows.

## 3. Fetch

A client on one of this user's devices asks for the list on startup. The server answers that client and sets `received_at`. A contact's device is not answered. A retry of the same fetch does not move `received_at` backward.

## 4. Push

When the list changes, the server pushes it to each online device of this user that has the client installed, and retries until that client confirms. Confirmation sets `received_at`. A device that is offline catches up with the fetch in step 3.

## 5. The page

The server serves a page that lists this user's devices and, for each contact, that contact's devices. Each row can be given an address or cleared. Saving writes the list, bumps `updated_at`, and runs the push from step 4.

## 6. The local interface

The client opens a TUN interface. It claims only the address assigned to this device. The other addresses in the list are routes to other devices. An empty assignment leaves the interface with nothing to claim. No packet is forwarded yet.

## 7. Forward and rewrite

A packet sent to an address in the list is stripped of its addresses and sent through pNet to the device that address names. The receiving client writes its own addresses onto the packet: the source is the address it assigned to the sender, and the destination is the address it assigned to itself. A reply is stripped and rewritten the same way on the way back. If this list has no address for the other device, the packet is dropped. TCP and UDP are the traffic this step carries.

## 8. Two of this user's devices

Run the server on the primary server-grade node and the client on two of this user's device-grade nodes. Assign an address to each. A packet from one address arrives on the other device as from the address in the shared list.

## 9. Two users

Run a server and a client for each of two users. Give the two lists different addresses for the same devices. A packet leaves one machine under that user's numbers and arrives on the other machine under the other user's numbers.
