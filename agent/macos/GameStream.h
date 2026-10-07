// RemoteMac's GameStream code in Rust (crates/rm-gamestream), linked as a static library.
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct RmPacketizer RmPacketizer;
RmPacketizer *rm_gs_packetizer_new(uint32_t packet_size, uint32_t fec_percentage, uint32_t min_fec_packets, uint32_t ssrc);
void rm_gs_packetizer_free(RmPacketizer *p);
void rm_gs_packetizer_set_fec(RmPacketizer *p, uint32_t fec_percentage);
uint8_t *rm_gs_packetize(RmPacketizer *p, const uint8_t *data, size_t len, uint32_t frame_index, bool idr, uint32_t timestamp, uint16_t latency, size_t *count);
void rm_gs_free(uint8_t *ptr, size_t len);

// Mac Desktop over full GameStream, reached through RemoteMac's connection (tunnel.rs)
typedef struct RmHostTunnel RmHostTunnel;
typedef void (*rm_gs_out)(void *ctx, int32_t kind, uint32_t id, const uint8_t *data, size_t len);
typedef struct {
    int32_t kind, a, b, c, d;
    uint8_t text[128];
} RmGsEvent;
const RmHostTunnel *rm_gs_desktop_start(const uint8_t *key, uint32_t fec_percentage, rm_gs_out cb, void *ctx, uint16_t *ports);
void rm_gs_desktop_stop(const RmHostTunnel *t);
bool rm_gs_desktop_frame(const RmHostTunnel *t, const uint8_t *data, size_t len, bool idr, uint64_t age_us);
bool rm_gs_desktop_poll(const RmHostTunnel *t, RmGsEvent *e);
void rm_gs_desktop_udp_in(const RmHostTunnel *t, uint8_t kind, const uint8_t *data, size_t len);
void rm_gs_desktop_tcp_open(const RmHostTunnel *t, uint32_t id);
void rm_gs_desktop_tcp_data(const RmHostTunnel *t, uint32_t id, const uint8_t *data, size_t len);
void rm_gs_desktop_tcp_close(const RmHostTunnel *t, uint32_t id);
const char *rm_gs_vk_name(uint16_t vk);
