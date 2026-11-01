# Security guards ship before the endpoints they protect

Every API story in this plan inherits the browser-facing guards (Host allow-list, Origin check, JSON content type plus `X-Holosmith-Client` on non-GET, no CORS headers) from a story that lands before any mutating endpoint exists. Likewise, network isolation, the egress proxy and the key proxy land before real agent turns, not after.

The normal order is to build features and add authorization later. We are not doing that: guards applied endpoint-by-endpoint afterwards are forgotten, and a guard added to a request path that already has twelve callers is a rewrite. Making the guard a middleware that a route cannot bypass means a new endpoint is secure by default and a story's acceptance criteria only ever test the feature.

Cost we accept: the first two days produce nothing visible beyond a `403`, and the demo of "create a session" arrives later than it otherwise would. Benefit: no endpoint in the system ever exists in an unguarded state, and no later story can regress security without failing a test that already existed.
