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
