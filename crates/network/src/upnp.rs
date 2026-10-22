use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    time::timeout,
};
use url::Url;

const PORT: u16 = 8787;
const LEASE_SECONDS: u32 = 7200;

pub struct RouterPortLease {
    control_url: Url,
    service: String,
    local_ip: IpAddr,
    mapped: bool,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl RouterPortLease {
    pub async fn open() -> Result<(String, Self), String> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|_| unavailable())?;
        let query = b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";
        socket
            .send_to(query, "239.255.255.250:1900")
            .await
            .map_err(|_| unavailable())?;
        let mut buf = [0u8; 4096];
        let response = timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
            .await
            .map_err(|_| "Роутер не ответил на запрос автоматической настройки. Включите UPnP в настройках роутера.".to_owned())?
            .map_err(|_| unavailable())?;
        let text = std::str::from_utf8(&buf[..response.0]).map_err(|_| unavailable())?;
        let location = header(text, "location").ok_or_else(unavailable)?;
        let description_url = Url::parse(location).map_err(|_| unavailable())?;
        if description_url.scheme() != "http"
            || !is_private_host(description_url.host_str().unwrap_or_default())
        {
            return Err(unavailable());
        }
        let local_ip = socket_local_ip(
            description_url.port_or_known_default().unwrap_or(80),
            description_url.host_str().unwrap_or_default(),
        )
        .await?;
        let description = http_request(&description_url, "GET", "", "").await?;
        let service = [
            "urn:schemas-upnp-org:service:WANIPConnection:2",
            "urn:schemas-upnp-org:service:WANIPConnection:1",
            "urn:schemas-upnp-org:service:WANPPPConnection:2",
            "urn:schemas-upnp-org:service:WANPPPConnection:1",
        ]
        .into_iter()
        .find(|service| description.contains(service))
        .ok_or_else(|| "Роутер найден, но не поддерживает автоматическое открытие портов UPnP.".to_owned())?;
        let control_path = service_control_url(&description, service).ok_or_else(unavailable)?;
        let control_url = description_url
            .join(&control_path)
            .map_err(|_| unavailable())?;
        if control_url.scheme() != "http" || control_url.host_str() != description_url.host_str() {
            return Err(unavailable());
        }

        let lease = Self {
            control_url,
            service: service.to_owned(),
            local_ip,
            mapped: false,
            task: None,
        };
        lease.add_mapping().await.map_err(|_| "Роутер найден, но не открыл порт 8787. Проверьте настройки UPnP или конфликт порта.".to_owned())?;
        let mut lease = lease;
        lease.mapped = true;
        let external_ip = lease.external_ip().await.map_err(|_| "Роутер открыл порт, но не сообщил внешний IP-адрес.".to_owned())?;
        let ip: IpAddr = external_ip.parse().map_err(|_| unavailable())?;
        if !is_public_ip(ip) {
            let _ = lease.remove_mapping().await;
            lease.mapped = false;
            return Err("Провайдер не выдаёт этому роутеру публичный IP (возможен CGNAT), поэтому сервер недоступен из интернета.".to_owned());
        }

        let control_url = lease.control_url.clone();
        let service = lease.service.clone();
        let local_ip = lease.local_ip;
        let task = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs((LEASE_SECONDS / 2) as u64));
            interval.tick().await;
            loop {
                interval.tick().await;
                let lease = Self {
                    control_url: control_url.clone(),
                    service: service.clone(),
                    local_ip,
                    mapped: false,
                    task: None,
                };
                let _ = lease.add_mapping().await;
            }
        });
        lease.task = Some(task);
        Ok((external_ip, lease))
    }

    async fn add_mapping(&self) -> Result<(), String> {
        self.soap("AddPortMapping", &format!(
            "<NewRemoteHost></NewRemoteHost><NewExternalPort>{PORT}</NewExternalPort><NewProtocol>TCP</NewProtocol><NewInternalPort>{PORT}</NewInternalPort><NewInternalClient>{}</NewInternalClient><NewEnabled>1</NewEnabled><NewPortMappingDescription>Branch Watch</NewPortMappingDescription><NewLeaseDuration>{LEASE_SECONDS}</NewLeaseDuration>", self.local_ip
        )).await.map(|_| ())
    }

    async fn external_ip(&self) -> Result<String, String> {
        let body = self.soap("GetExternalIPAddress", "").await?;
        xml_value(&body, "NewExternalIPAddress").ok_or_else(unavailable)
    }

    async fn remove_mapping(&self) -> Result<(), String> {
        self.soap("DeletePortMapping", &format!("<NewRemoteHost></NewRemoteHost><NewExternalPort>{PORT}</NewExternalPort><NewProtocol>TCP</NewProtocol>")).await.map(|_| ())
    }

    async fn soap(&self, action: &str, args: &str) -> Result<String, String> {
        let body = format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{}\">{args}</u:{action}></s:Body></s:Envelope>",
            self.service
        );
        let headers = format!(
            "Content-Type: text/xml; charset=\"utf-8\"\r\nSOAPAction: \"{}#{action}\"\r\n",
            self.service
        );
        let response = http_request(&self.control_url, "POST", &headers, &body).await?;
        if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
            return Err(unavailable());
        }
        Ok(response)
    }
}

impl Drop for RouterPortLease {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if self.mapped {
            let lease = Self {
                control_url: self.control_url.clone(),
                service: self.service.clone(),
                local_ip: self.local_ip,
                mapped: false,
                task: None,
            };
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = lease.remove_mapping().await;
                });
            }
        }
    }
}

async fn socket_local_ip(port: u16, router: &str) -> Result<IpAddr, String> {
    let router: IpAddr = router.parse().map_err(|_| unavailable())?;
    let route = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|_| unavailable())?;
    route
        .connect(SocketAddr::new(router, port))
        .await
        .map_err(|_| unavailable())?;
    let ip = route.local_addr().map_err(|_| unavailable())?.ip();
    Ok(ip)
}

async fn http_request(
    url: &Url,
    method: &str,
    headers: &str,
    body: &str,
) -> Result<String, String> {
    let host = url.host_str().ok_or_else(unavailable)?;
    let port = url.port_or_known_default().ok_or_else(unavailable)?;
    let mut stream = timeout(Duration::from_secs(3), TcpStream::connect((host, port)))
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    let path = if url.query().is_some() {
        format!("{}?{}", url.path(), url.query().unwrap_or_default())
    } else {
        url.path().to_owned()
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| unavailable())?;
    let mut bytes = Vec::new();
    let mut limited = stream.take(128 * 1024);
    timeout(Duration::from_secs(3), limited.read_to_end(&mut bytes))
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    let response = String::from_utf8(bytes).map_err(|_| unavailable())?;
    if response
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return decode_chunked(&response);
    }
    Ok(response)
}

fn decode_chunked(response: &str) -> Result<String, String> {
    let (headers, mut body) = response.split_once("\r\n\r\n").ok_or_else(unavailable)?;
    let mut decoded = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).map_err(|_| unavailable())?;
        if size == 0 {
            break;
        }
        if rest.len() < size + 2 {
            return Err(unavailable());
        }
        decoded.push_str(&rest[..size]);
        body = &rest[size + 2..];
    }
    Ok(format!("{headers}\r\n\r\n{decoded}"))
}

fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn service_control_url(xml: &str, service: &str) -> Option<String> {
    for part in xml.split("<service>").skip(1) {
        let part = part.split("</service>").next()?;
        let Some(service_type) = xml_value(part, "serviceType") else {
            continue;
        };
        if service_type == service {
            return xml_value(part, "controlURL");
        }
    }
    None
}

fn xml_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let (_, rest) = xml.split_once(&open)?;
    let (value, _) = rest.split_once(&close)?;
    Some(value.trim().to_owned())
}

fn is_private_host(host: &str) -> bool {
    host.parse::<IpAddr>()
        .map(|ip| match ip {
            IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
            IpAddr::V6(ip) => {
                ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
            }
        })
        .unwrap_or(false)
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            let carrier_nat = octets[0] == 100 && (64..=127).contains(&octets[1]);
            let benchmarking = octets[0] == 198 && (18..=19).contains(&octets[1]);
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_broadcast()
                && !ip.is_multicast()
                && !carrier_nat
                && !benchmarking
        }
        IpAddr::V6(ip) => {
            !ip.is_loopback()
                && !ip.is_unique_local()
                && !ip.is_unicast_link_local()
                && !ip.is_unspecified()
        }
    }
}

fn unavailable() -> String {
    "Не удалось настроить сетевой сервер. Проверьте подключение и настройки роутера.".into()
}
