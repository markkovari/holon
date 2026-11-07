This interface defines a handler of HTTP Requests.

In a `wasi:http/service` this interface is exported to respond to an
incoming HTTP Request with a Response.

In `wasi:http/middleware` this interface is both exported and imported as
the "downstream" and "upstream" directions of the middleware chain.