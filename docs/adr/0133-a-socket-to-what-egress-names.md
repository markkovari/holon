# ADR-0133 — A socket to what egress names

*A component could reach the network only through `wasi:http`, so anything speaking another
protocol needed a native relay. The host now lets it open a TCP connection to an address its egress
names, and to nothing else.*

**Status: accepted.**

## The problem

`comp-host` linked `wasi:sockets` (it comes with the p2 linker) but built every guest's WASI with
the default policy, which refuses every address. So `mail-http` could only `POST`, and speaking
SMTP to MailHog took `comp-mailrelay`: a native daemon whose whole job was turning that POST into an
SMTP session. ADR-0095 admits a native thing for "a raw socket"; a TCP connection to a named
server is not that, once the host can bound it.

## The decision

- **A guest may `connect` over TCP to an address the egress policy allows** (`wasi_ctx` in
  `host/src/main.rs`, `EgressPolicy::socket_allowed`). Both of HTTP's checks hold: the address one
  (no private ranges without `--allow-private-egress`, never the node's own listener or NATS), and
  the allow-list, which for a socket must name the port: an entry `host:port` allows `port` on
  `host`'s addresses, a name being resolved for the check. The bare host HTTP also accepts is not
  enough for a socket, which could otherwise reach any port.
- **No bind, no UDP.** A guest connects out; it does not listen, and it has no datagrams. Name
  lookup is on, so `std::net::TcpStream::connect("host:port")` works in a guest.
- **`mail-http` speaks SMTP to `mail:smtp`** when that is set (`host:port` on its egress), and
  refuses a line break in an address or subject, which would otherwise start a new command or
  header. `comp-mailrelay` is deleted; the gates point the component at their SMTP sink directly.

## Consequences

- A component can speak any TCP protocol to a server it was given, under the same allow-list as
  its HTTP: SMTP, a database's wire protocol, a NATS client. TLS is the component's own.
- `std` on wasip2 has no `TcpStream::try_clone`: one stream is read and written through `&TcpStream`.
- An allow-list entry for HTTP now also grants a TCP connect to that same `host:port`. That is the
  same reach the HTTP door already gave to that port, not a new one.
