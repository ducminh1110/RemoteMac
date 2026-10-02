// Virtual display (the technique BetterDummy uses): CoreGraphics' CGVirtualDisplay classes are
// private, so they are declared here and looked up at run time. Exposed to Swift as plain C.
#include <stdint.h>

/// Creates (or re-creates) the agent's virtual display with one mode of `width`x`height`
/// points (`hidpi`: backed by 2x pixels). Returns its CGDirectDisplayID, 0 if unavailable;
/// `err` (may be NULL) receives a reason.
uint32_t rm_virtual_display_create(uint32_t width, uint32_t height, int hidpi, char *err, int errlen);
void rm_virtual_display_destroy(void);
