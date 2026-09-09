def run(api, login):
    admin = login("admin@example.test", "adminpass123", ["admin"])
    alice = login("alice@example.test", "alicepass123", ["responder"])
    bob = login("bob@example.test", "bobpass123", ["responder"])

    code, body = api("POST", "/api/incidents", {"title": "prod down", "severity": "high"}, token=alice)
    assert code == 201, ("create incident", code, body)
    iid = body["id"]

    code, body = api("POST", f"/api/incidents/{iid}/resolve", {}, token=alice)
    assert code == 409, ("resolve unassigned incident", code, body)

    code, body = api("POST", f"/api/incidents/{iid}/assign", {"responder": "irrelevant"}, token=alice)
    assert code == 403, ("non-admin assigns", code, body)

    code, body = api("POST", f"/api/incidents/{iid}/assign", {"responder": alice["subject"]}, token=admin)
    assert code == 200, ("admin assigns alice", code, body)

    code, body = api("POST", f"/api/incidents/{iid}/resolve", {}, token=bob)
    assert code == 403, ("bob (not assigned) resolves", code, body)

    code, body = api("POST", f"/api/incidents/{iid}/resolve", {}, token=alice)
    assert code == 200, ("alice (assigned) resolves", code, body)

    code, body = api("POST", "/api/incidents", {"title": "leak", "severity": "low"}, token=bob)
    iid2 = body["id"]
    code, body = api("POST", f"/api/incidents/{iid2}/assign", {"responder": bob["subject"]}, token=admin)
    assert code == 200
    code, body = api("POST", f"/api/incidents/{iid2}/resolve", {}, token=admin)
    assert code == 200, ("admin override resolves someone else's incident", code, body)
