use actix::{Actor, ActorContext, Addr, Handler, Message, StreamHandler};
use actix_web::{middleware, web, App, Error, HttpRequest, HttpResponse, HttpServer, Resource};
use actix_web_actors::ws;
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Deserialize)]
struct PathChannel {
    channel: String,
}

enum ControlMessage {
    Connected(Addr<Connection>),
    Transmit(Result<ws::Message, ws::ProtocolError>),
    Listener,
    Dialer,
}
impl Message for ControlMessage {
    type Result = ();
}
enum Connection {
    WaitingPeer(Vec<Result<ws::Message, ws::ProtocolError>>),
    Connected(Addr<Connection>),
}
impl Connection {
    fn new() -> Connection {
        Connection::WaitingPeer(Vec::new())
    }
}
impl Actor for Connection {
    type Context = ws::WebsocketContext<Self>;
}
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for Connection {
    fn handle(&mut self, item: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match self {
            Connection::WaitingPeer(pending) => {
                match item {
                    Ok(ws::Message::Close(_)) | Err(_) => {
                        ctx.stop();
                    }
                    _ => {}
                }
                pending.push(item);
            }
            Connection::Connected(peer) => {
                match item {
                    Ok(ws::Message::Close(_)) | Err(_) => {
                        ctx.stop();
                    }
                    _ => {}
                }
                peer.do_send(ControlMessage::Transmit(item));
            }
        }
    }
}
impl Handler<ControlMessage> for Connection {
    type Result = ();

    fn handle(&mut self, msg: ControlMessage, ctx: &mut Self::Context) -> Self::Result {
        match msg {
            ControlMessage::Connected(peer) => {
                let mut pending = Vec::new();
                match self {
                    Connection::WaitingPeer(pending2) => {
                        std::mem::swap(&mut pending, pending2);
                    }
                    Connection::Connected(_) => {}
                }
                pending
                    .into_iter()
                    .for_each(|msg| peer.do_send(ControlMessage::Transmit(msg)));
                let mut new_self = Connection::Connected(peer);
                std::mem::swap(self, &mut new_self);
            }
            ControlMessage::Transmit(msg) => match msg {
                Ok(ws::Message::Binary(data)) => ctx.binary(data),
                Ok(ws::Message::Text(text)) => ctx.text(text),
                Ok(ws::Message::Ping(msg)) => ctx.ping(&msg),
                Ok(ws::Message::Pong(msg)) => ctx.pong(&msg),
                Ok(ws::Message::Close(reason)) => {
                    ctx.close(reason);
                    ctx.stop();
                }
                _ => {
                    ctx.stop();
                }
            },
            ControlMessage::Listener => ctx.text("LISTENER"),
            ControlMessage::Dialer => ctx.text("DIALER"),
        }
    }
}

struct Signaling {
    half_channels: HashMap<String, Addr<Connection>>,
}
impl Signaling {
    fn scope(obj: Arc<Mutex<Self>>) -> Resource {
        web::resource("{channel}").route(web::get().to(
            move |req, stream, path: web::Path<PathChannel>| {
                let obj = obj.clone();
                async move {
                    obj.lock()
                        .unwrap()
                        .ws_index(req, stream, &path.channel)
                        .await
                }
            },
        ))
    }

    fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Signaling {
            half_channels: Default::default(),
        }))
    }

    async fn ws_index(
        &mut self,
        req: HttpRequest,
        stream: web::Payload,
        channel: &str,
    ) -> Result<HttpResponse, Error> {
        let conn = Connection::new();
        let (addr, res) = ws::start_with_addr(conn, &req, stream)?;

        self.new_connection(addr, channel);

        Ok(res)
    }

    fn new_connection(&mut self, user: Addr<Connection>, channel: &str) {
        match self.half_channels.entry(channel.into()) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let peer = entry.remove();
                if !peer.connected() {
                    return self.new_connection(user, channel);
                }

                peer.do_send(ControlMessage::Connected(user.clone()));
                user.do_send(ControlMessage::Connected(peer));
                user.do_send(ControlMessage::Dialer);
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                user.do_send(ControlMessage::Listener);
                entry.insert(user);
            }
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    std::env::set_var("RUST_LOG", "actix_server=info,actix_web=info");
    env_logger::init();

    let bind = std::env::var("BIND").unwrap_or("127.0.0.1:8080".to_string());

    let signal = Signaling::new();
    HttpServer::new(move || {
        App::new()
            .wrap(middleware::Logger::default())
            .service(web::scope("/signaling").service(Signaling::scope(signal.clone())))
            .service(web::scope("/signalling").service(Signaling::scope(signal.clone())))
    })
    .bind(&bind)?
    .run()
    .await
}
