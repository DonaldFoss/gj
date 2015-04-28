extern crate gj;

#[test]
fn hello() {
    ::gj::EventLoop::init();
    let (promise, mut fulfiller) = ::gj::new_promise_and_fulfiller::<u32>();
    let p1 = promise.then(|x| {
        assert_eq!(x, 10);
        return Ok(x + 1);
    }, |e| {
        return Err(e);
    });

    fulfiller.fulfill(10);
    let value = p1.wait().unwrap();
    assert_eq!(value, 11);

}


