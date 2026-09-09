def run(api, login):
    lib = login("lib@example.test", "libpass123", ["librarian"])
    alice = login("alice@example.test", "alicepass123", ["patron"])
    bob = login("bob@example.test", "bobpass123", ["patron"])

    code, body = api("POST", "/api/books", {"title": "The Left Hand of Darkness"}, token=alice)
    assert code == 403, ("patron adds book", code, body)

    code, body = api("POST", "/api/books", {"title": "The Left Hand of Darkness"}, token=lib)
    assert code == 201, ("librarian adds book", code, body)
    bkid = body["id"]
    assert body.get("call_number", "").startswith("BK-"), ("call number minted", body)

    code, body = api("POST", f"/api/books/{bkid}/borrow", {}, token=alice)
    assert code == 201, ("alice borrows", code, body)
    lid = body["id"]

    code, body = api("POST", f"/api/books/{bkid}/borrow", {}, token=bob)
    assert code == 409, ("bob borrows an already-borrowed book", code, body)

    code, body = api("POST", f"/api/loans/{lid}/return", {}, token=bob)
    assert code == 403, ("bob returns alice's loan", code, body)

    code, body = api("POST", f"/api/loans/{lid}/return", {}, token=alice)
    assert code == 200, ("alice returns her own loan", code, body)

    code, body = api("POST", f"/api/books/{bkid}/borrow", {}, token=bob)
    assert code == 201, ("bob borrows after return", code, body)
    lid2 = body["id"]

    code, body = api("POST", f"/api/loans/{lid2}/return", {}, token=lib)
    assert code == 200, ("librarian override returns bob's loan", code, body)
