def run(api, login):
    admin = login("admin@example.test", "adminpass123", ["admin"])
    alice = login("alice@example.test", "alicepass123", ["member"])
    bob = login("bob@example.test", "bobpass123", ["member"])

    code, body = api("POST", "/api/rooms", {"name": "Ada"}, token=alice)
    assert code == 403, ("member creates room", code, body)
    code, body = api("POST", "/api/rooms", {"name": "Ada"}, token=admin)
    assert code == 201, ("admin creates room", code, body)
    rid = body["id"]

    code, body = api("POST", f"/api/rooms/{rid}/book", {"date": "2030-01-01"}, token=alice)
    assert code == 201, ("alice books", code, body)
    bid = body["id"]

    code, body = api("POST", f"/api/rooms/{rid}/book", {"date": "2030-01-01"}, token=bob)
    assert code == 409, ("bob double-books same room/date", code, body)

    code, body = api("POST", f"/api/bookings/{bid}/cancel", {}, token=bob)
    assert code == 403, ("bob cancels alice's booking", code, body)

    code, body = api("POST", f"/api/bookings/{bid}/cancel", {}, token=alice)
    assert code == 200, ("alice cancels own booking", code, body)

    code, body = api("POST", f"/api/rooms/{rid}/book", {"date": "2030-01-01"}, token=bob)
    assert code == 201, ("bob books after cancel freed the slot", code, body)

    code, body = api("POST", f"/api/rooms/{rid}/book", {"date": "2030-01-02"}, token=alice)
    assert code == 201
    code, body = api("POST", f"/api/rooms/{rid}/book", {"date": "2030-01-03"}, token=alice)
    assert code == 429, ("alice exceeds daily booking quota", code, body)
