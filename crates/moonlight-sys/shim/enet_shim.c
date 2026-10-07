// A few flat functions over ENet for the host side (crates/rm-gamestream): the Rust code never
// touches ENet's structs, whose layout depends on the platform (sockaddr_storage, SOCKET).
#include <enet/enet.h>
#include <string.h>

int rm_enet_init(void) { return enet_initialize(); }

// A server on `port` (all interfaces), up to `peers` clients, 1 channel per... as many as asked.
ENetHost* rm_enet_server(unsigned short port, int ipv6, size_t peers, size_t channels) {
    ENetAddress addr;
    memset(&addr, 0, sizeof(addr));
    if (ipv6) {
        struct sockaddr_in6* a = (struct sockaddr_in6*)&addr.address;
        a->sin6_family = AF_INET6;
        a->sin6_addr = in6addr_any;
        addr.addressLength = sizeof(struct sockaddr_in6);
    } else {
        struct sockaddr_in* a = (struct sockaddr_in*)&addr.address;
        a->sin_family = AF_INET;
        a->sin_addr.s_addr = INADDR_ANY;
        addr.addressLength = sizeof(struct sockaddr_in);
    }
    enet_address_set_port(&addr, port);
    return enet_host_create(ipv6 ? AF_INET6 : AF_INET, &addr, peers, channels, 0, 0);
}

// The port the host is bound to (0 on error).
unsigned short rm_enet_port(ENetHost* host) {
    ENetAddress a;
    if (enet_socket_get_address(host->socket, &a) < 0) return 0;
    if (((struct sockaddr*)&a.address)->sa_family == AF_INET6) return ntohs(((struct sockaddr_in6*)&a.address)->sin6_port);
    return ntohs(((struct sockaddr_in*)&a.address)->sin_port);
}

// One event: returns its type (0 none, 1 connect, 2 disconnect, 3 receive) or <0 on error.
// For receive, *packet must be released with rm_enet_packet_free.
int rm_enet_service(ENetHost* host, unsigned int timeout_ms, ENetPeer** peer, unsigned int* data, ENetPacket** packet) {
    ENetEvent ev;
    int r = enet_host_service(host, &ev, timeout_ms);
    if (r <= 0) return r;
    *peer = ev.peer;
    *data = ev.data;
    *packet = ev.packet;
    return (int)ev.type;
}

const unsigned char* rm_enet_packet_data(ENetPacket* p, size_t* len) { *len = p->dataLength; return p->data; }
void rm_enet_packet_free(ENetPacket* p) { enet_packet_destroy(p); }

int rm_enet_send(ENetPeer* peer, unsigned char channel, const void* data, size_t len, int reliable) {
    ENetPacket* p = enet_packet_create(data, len, reliable ? ENET_PACKET_FLAG_RELIABLE : 0);
    if (p == NULL) return -1;
    if (enet_peer_send(peer, channel, p) < 0) { enet_packet_destroy(p); return -1; }
    return 0;
}

void rm_enet_flush(ENetHost* host) { enet_host_flush(host); }
void rm_enet_disconnect_now(ENetPeer* peer) { enet_peer_disconnect_now(peer, 0); }
void rm_enet_destroy(ENetHost* host) { enet_host_destroy(host); }

// A client connected to `ip`:`port` (numeric address), waiting up to `timeout_ms` for the
// connection. Returns NULL on failure; *peer is the server.
ENetHost* rm_enet_client(const char* ip, unsigned short port, size_t channels, unsigned int connect_data, unsigned int timeout_ms, ENetPeer** peer) {
    ENetAddress addr;
    memset(&addr, 0, sizeof(addr));
    if (enet_address_set_host(&addr, ip) < 0) return NULL;
    enet_address_set_port(&addr, port);
    ENetHost* h = enet_host_create(((struct sockaddr*)&addr.address)->sa_family, NULL, 1, channels, 0, 0);
    if (h == NULL) return NULL;
    ENetPeer* p = enet_host_connect(h, &addr, channels, connect_data);
    if (p == NULL) { enet_host_destroy(h); return NULL; }
    ENetEvent ev;
    if (enet_host_service(h, &ev, timeout_ms) <= 0 || ev.type != ENET_EVENT_TYPE_CONNECT) {
        enet_peer_reset(p);
        enet_host_destroy(h);
        return NULL;
    }
    enet_host_flush(h);
    *peer = p;
    return h;
}
