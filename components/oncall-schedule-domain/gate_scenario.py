def run(api, login):
    lead = login("lead@example.test", "leadpass123", ["lead"])
    alice = login("alice@example.test", "alicepass123", ["engineer"])
    bob = login("bob@example.test", "bobpass123", ["engineer"])

    code, body = api("POST", "/api/shifts", {"engineer": alice["subject"], "starts_at": 1234567890}, token=lead)
    assert code == 201, ("lead schedules alice", code, body)
    sid = body["id"]

    code, body = api("POST", f"/api/shifts/{sid}/reassign", {"engineer": bob["subject"]}, token=bob)
    assert code == 403, ("bob reassigns alice's shift", code, body)

    code, body = api("POST", f"/api/shifts/{sid}/reassign", {"engineer": bob["subject"]}, token=alice)
    assert code == 200, ("alice (owner) reassigns her own shift to bob", code, body)

    code, body = api("POST", f"/api/shifts/{sid}/reassign", {"engineer": alice["subject"]}, token=lead)
    assert code == 200, ("lead overrides and reassigns back to alice", code, body)
