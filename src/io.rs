// Copyright (c) 2013-2015 Sandstorm Development Group, Inc. and contributors
// Licensed under the MIT License:
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.

use std::rc::Rc;
use std::cell::RefCell;
use std::ops::{DerefMut, Deref};
use mio::util::Slab;
use mio::Socket;
use {EventPort, Promise, PromiseFulfiller, Result, new_promise_and_fulfiller};
use private::{with_current_event_loop};

pub trait AsyncRead {
    fn read<T>(self: Self, buf: T, min_bytes: usize, max_bytes: usize) -> Promise<(Self, T, usize)>
        where T: DerefMut<Target=[u8]>;
}

pub trait AsyncWrite {
    fn write<T>(self: Self, buf: T) -> Promise<(Self, T)>
        where T: Deref<Target=[u8]>;
}

#[derive(Copy, Clone)]
pub struct NetworkAddress {
    address: ::std::net::SocketAddr,
}

impl NetworkAddress {
    pub fn new(address: ::std::net::SocketAddr) -> NetworkAddress {
        NetworkAddress { address: address }
    }

    pub fn listen(self) -> Result<ConnectionReceiver> {
        let socket = try!(::mio::tcp::TcpSocket::v4());
        try!(socket.set_reuseaddr(true));
        try!(socket.bind(&self.address));
        let token = FdObserver::new();
        Ok(ConnectionReceiver { listener: try!(socket.listen(256)),
                                token: token })
    }

    pub fn connect(self) -> Promise<(AsyncOutputStream, AsyncInputStream)> {
        let socket = ::mio::tcp::TcpSocket::v4().unwrap();
        let token = FdObserver::new();
        let (stream, _) = socket.connect(&self.address).unwrap();
        with_current_event_loop(move |event_loop| {
            event_loop.event_port.borrow_mut().reactor.register_opt(&stream, token,
                                                                    ::mio::Interest::writable(),
                                                                    ::mio::PollOpt::edge()|
                                                                    ::mio::PollOpt::oneshot()).unwrap();
            let promise =
                event_loop.event_port.borrow_mut().handler.observers[token].when_becomes_writable();
            return promise.map(move |()| {
                // TODO check for error.

                return with_current_event_loop(move |event_loop| {
                    event_loop.event_port.borrow_mut().reactor.reregister(
                        &stream, token,
                        ::mio::Interest::writable() | ::mio::Interest::readable(),
                        ::mio::PollOpt::edge()).unwrap();
                    return Ok(AsyncIo::new(stream, token));
                });

            });
        })
    }
}


pub struct ConnectionReceiver {
    listener: ::mio::tcp::TcpListener,
    token: ::mio::Token,
}

impl Drop for ConnectionReceiver {
    fn drop(&mut self) {
        // deregister the token
    }
}

impl ConnectionReceiver {
    pub fn accept(self) -> Promise<(ConnectionReceiver, (AsyncOutputStream, AsyncInputStream))> {
        with_current_event_loop(move |event_loop| {
            let promise =
                event_loop.event_port.borrow_mut().handler.observers[self.token].when_becomes_readable();
            event_loop.event_port.borrow_mut().reactor.register_opt(&self.listener, self.token,
                                                                    ::mio::Interest::readable(),
                                                                    ::mio::PollOpt::edge() |
                                                                    ::mio::PollOpt::oneshot()).unwrap();
            return promise.map(move |()| {
                let stream = try!(self.listener.accept()).unwrap();
                let token = FdObserver::new();

                return with_current_event_loop(move |event_loop| {
                    event_loop.event_port.borrow_mut().reactor.reregister(
                        &stream, token,
                        ::mio::Interest::writable() | ::mio::Interest::readable(),
                        ::mio::PollOpt::edge()).unwrap();
                    return Ok((self, AsyncIo::new(stream, token)));
                });
            });
        })
    }
}

pub struct AsyncInputStream {
    hub: Rc<RefCell<AsyncIo>>
}

pub struct AsyncOutputStream {
    hub: Rc<RefCell<AsyncIo>>
}

struct AsyncIo {
    stream: ::mio::tcp::TcpStream,
    token: ::mio::Token,
}

impl AsyncIo {
    fn new(stream: ::mio::tcp::TcpStream, token: ::mio::Token) -> (AsyncOutputStream, AsyncInputStream) {
        let hub = Rc::new(RefCell::new(AsyncIo { stream: stream, token: token }));

        (AsyncOutputStream { hub: hub.clone() }, AsyncInputStream { hub: hub } )
    }
}

impl AsyncInputStream {
    fn read_internal<T>(self,
                        mut buf: T,
                        start: usize,
                        min_bytes: usize,
                        max_bytes: usize) -> Promise<(Self, T, usize)> where T: DerefMut<Target=[u8]> {
        if start >= min_bytes {
            return Promise::fulfilled((self, buf, start));
        } else {
            with_current_event_loop(move |event_loop| {
                let promise =
                    event_loop.event_port.borrow_mut().handler.observers[self.hub.borrow().token].when_becomes_readable();
                return promise.then(move |()| {
                    use mio::TryRead;
                    let n = try!(self.hub.borrow_mut().stream.read_slice(&mut buf[start..])).unwrap();
                    return Ok(self.read_internal(buf, start + n, min_bytes, max_bytes));
                });
            })
        }
    }
}

impl AsyncOutputStream {
    fn write_internal<T>(self,
                         buf: T,
                         cursor: usize) -> Promise<(Self, T)> where T: Deref<Target=[u8]> {
        if cursor == buf.len() {
            return Promise::fulfilled((self, buf));
        } else {
            with_current_event_loop(move |event_loop| {
                let promise =
                    event_loop.event_port.borrow_mut().handler.observers[self.hub.borrow().token].when_becomes_writable();
                return promise.then(move |()| {
                    use mio::TryWrite;
                    let n = try!(self.hub.borrow_mut().stream.write_slice(&buf[cursor..])).unwrap();
                    return Ok(self.write_internal(buf, cursor + n));
                });
            })
        }
    }
}

impl AsyncRead for AsyncInputStream {
    fn read<T>(self, buf: T,
               min_bytes: usize,
               max_bytes: usize) -> Promise<(Self, T, usize)> where T: DerefMut<Target=[u8]> {
        self.read_internal(buf, 0, min_bytes, max_bytes)
    }
}

impl AsyncWrite for AsyncOutputStream {
    fn write<T>(self, buf: T) -> Promise<(Self, T)> where T: Deref<Target=[u8]> {
        self.write_internal(buf, 0)
    }
}


pub struct FdObserver {
    read_fulfiller: Option<Box<PromiseFulfiller<()>>>,
    write_fulfiller: Option<Box<PromiseFulfiller<()>>>,
}

impl FdObserver {
    pub fn new() -> ::mio::Token {
        with_current_event_loop(move |event_loop| {

            let observer = FdObserver { read_fulfiller: None, write_fulfiller: None };
            let event_port = &mut *event_loop.event_port.borrow_mut();
            let token = match event_port.handler.observers.insert(observer) {
                Ok(tok) => tok,
                Err(_obs) => panic!("could not insert observer."),
            };
            return token;
        })
    }

    pub fn when_becomes_readable(&mut self) -> Promise<()> {
        let (promise, fulfiller) = new_promise_and_fulfiller();
        self.read_fulfiller = Some(fulfiller);
        return promise;
    }

    pub fn when_becomes_writable(&mut self) -> Promise<()> {
        let (promise, fulfiller) = new_promise_and_fulfiller();
        self.write_fulfiller = Some(fulfiller);
        return promise;
    }
}

pub struct MioEventPort {
    handler: Handler,
    reactor: ::mio::EventLoop<Handler>,
}

pub struct Handler {
    observers: Slab<FdObserver>,
}

impl MioEventPort {
    pub fn new() -> Result<MioEventPort> {
        Ok(MioEventPort {
            handler: Handler { observers: Slab::new(100) },
            reactor: try!(::mio::EventLoop::new()),
        })
    }
}

impl ::mio::Handler for Handler {
    type Timeout = ();
    type Message = ();
    fn readable(&mut self, _event_loop: &mut ::mio::EventLoop<Handler>,
                token: ::mio::Token, _hint: ::mio::ReadHint) {
        match ::std::mem::replace(&mut self.observers[token].read_fulfiller, None) {
            Some(fulfiller) => {
                fulfiller.fulfill(())
            }
            None => {
                ()
            }
        }
    }
    fn writable(&mut self, _event_loop: &mut ::mio::EventLoop<Handler>, token: ::mio::Token) {
        match ::std::mem::replace(&mut self.observers[token].write_fulfiller, None) {
            Some(fulfiller) => fulfiller.fulfill(()),
            None => (),
        }
    }
}

impl EventPort for MioEventPort {
    fn wait(&mut self) -> bool {
        self.reactor.run_once(&mut self.handler).unwrap();
        return false;
    }

    fn poll(&mut self) -> bool {
        self.reactor.run_once(&mut self.handler).unwrap();
        return false;
    }
}
