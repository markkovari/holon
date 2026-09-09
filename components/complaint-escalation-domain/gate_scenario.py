def run(api, login):
    sup = login("sup@example.test", "suppass123", ["supervisor"])
    alice = login("alice@example.test", "alicepass123", ["agent"])
    bob = login("bob@example.test", "bobpass123", ["agent"])

    code, body = api("POST", "/api/complaints", {"summary": "billing error"}, token=alice)
    assert code == 201, ("create", code, body)
    cid = body["id"]

    code, body = api("POST", f"/api/complaints/{cid}/escalate", {}, token=alice)
    assert code == 409, ("escalate unassigned", code, body)

    code, body = api("POST", f"/api/complaints/{cid}/assign", {"agent": "x"}, token=alice)
    assert code == 403, ("non-supervisor assigns", code, body)

    code, body = api("POST", f"/api/complaints/{cid}/assign", {"agent": alice["subject"]}, token=sup)
    assert code == 200, ("supervisor assigns alice", code, body)

    code, body = api("POST", f"/api/complaints/{cid}/escalate", {}, token=bob)
    assert code == 403, ("bob (not assigned) escalates", code, body)

    code, body = api("POST", f"/api/complaints/{cid}/escalate", {}, token=alice)
    assert code == 200, ("alice (assigned) escalates", code, body)

    code, body = api("POST", f"/api/complaints/{cid}/resolve", {}, token=sup)
    assert code == 200, ("supervisor override resolves", code, body)
