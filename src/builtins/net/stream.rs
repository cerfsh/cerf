use std::io;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Copy bytes in both directions until either side closes.
///
/// When stdin reaches EOF, only the socket write half is shut down so the
/// remote side can still finish its response.
pub(crate) async fn bridge_tcp<R, W>(
    stream: TcpStream,
    mut input: R,
    mut output: W,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = stream.into_split();

    let outbound = async {
        tokio::io::copy(&mut input, &mut writer).await?;
        writer.shutdown().await
    };
    let inbound = async {
        tokio::io::copy(&mut reader, &mut output).await?;
        output.flush().await
    };

    tokio::pin!(inbound);
    tokio::select! {
        result = &mut inbound => result,
        result = outbound => {
            result?;
            inbound.await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn bridges_binary_data_and_half_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            stream
                .read_to_end(&mut request)
                .await
                .expect("read request");
            assert_eq!(request, b"a\0b");
            stream.write_all(b"reply").await.expect("write reply");
            stream.shutdown().await.expect("shutdown");
        });

        let stream = TcpStream::connect(address).await.expect("connect");
        let mut output = Vec::new();
        bridge_tcp(stream, &b"a\0b"[..], &mut output)
            .await
            .expect("bridge");

        assert_eq!(output, b"reply");
        server.await.expect("server task");
    }
}
