#ifndef OBLAS_COMMON_H
#define OBLAS_COMMON_H

#include <stddef.h>
#include <stdint.h>

#if defined(__x86_64__) || defined(_M_X64) || defined(__i386__) || defined(_M_IX86)
#define OBLAS_ARCH_X86 1
#elif defined(__aarch64__) || defined(_M_ARM64) || defined(__arm__) || defined(_M_ARM)
#define OBLAS_ARCH_ARM 1
#elif defined(__riscv)
#define OBLAS_ARCH_RISCV 1
#endif

#if defined(_MSC_VER) && !defined(__clang__)
#include <intrin.h>
#include <stdlib.h>
#include <string.h>
#include <malloc.h>

#define __builtin_bswap64 _byteswap_uint64
#define __builtin_bswap32 _byteswap_ulong
#define __attribute__(x)
#define restrict __restrict

static inline int __builtin_popcount(unsigned int x)
{
#if defined(_M_X64) || defined(_M_IX86)
    return (int)__popcnt(x);
#else
    /* Portable SW fallback */
    x = x - ((x >> 1) & 0x55555555);
    x = (x & 0x33333333) + ((x >> 2) & 0x33333333);
    return (((x + (x >> 4)) & 0x0F0F0F0F) * 0x01010101) >> 24;
#endif
}

static inline int __builtin_ctz(unsigned int x)
{
    unsigned long index;
    if (_BitScanForward(&index, x)) {
        return (int)index;
    }
    return 32;
}

#if defined(OBLAS_ARCH_X86)
/* The extended register state the OS has enabled (XCR0), or 0 when it manages none (no OSXSAVE).
 * CPUID alone says what the processor can do, not what the OS (or the hypervisor) lets a program
 * use: a guest can advertise AVX-512 with XCR0 = 0x7, and the first zmm instruction then faults
 * with an illegal instruction. */
static inline unsigned long long oblas_xcr0(void)
{
    int info[4] = {0};
    __cpuid(info, 1);
    if (!(info[2] & (1 << 27)))
        return 0;
    return _xgetbv(0);
}

/* MSVC has no __builtin_cpu_supports: this one answers for the features oblas_lite.c asks about,
 * with the OS state checked as GCC and Clang do. "avx512f" stands for every AVX-512 subset the
 * kernels are built for (F, BW, DQ, VL). */
static inline int __builtin_cpu_supports(const char *feature)
{
    int info[4] = {0};
    unsigned ecx1 = 0, ebx7 = 0, ecx7 = 0;
    __cpuid(info, 0);
    int max_leaf = info[0];
    if (max_leaf >= 1) {
        __cpuid(info, 1);
        ecx1 = (unsigned)info[2];
    }
    if (max_leaf >= 7) {
        __cpuidex(info, 7, 0);
        ebx7 = (unsigned)info[1];
        ecx7 = (unsigned)info[2];
    }
    unsigned long long xcr0 = oblas_xcr0();
    /* AVX and the XMM|YMM state */
    int ymm = (ecx1 & (1u << 28)) && (xcr0 & 0x6) == 0x6;
    /* and the opmask, ZMM_Hi256 and Hi16_ZMM state */
    int zmm = ymm && (xcr0 & 0xE6) == 0xE6;

    if (strcmp(feature, "ssse3") == 0)
        return (ecx1 & (1u << 9)) != 0;
    if (strcmp(feature, "avx2") == 0)
        return ymm && (ebx7 & (1u << 5)) != 0;
    if (strcmp(feature, "avx512f") == 0)
        return zmm && (ebx7 & (1u << 16)) && (ebx7 & (1u << 17)) && (ebx7 & (1u << 30)) && (ebx7 & (1u << 31));
    if (strcmp(feature, "gfni") == 0)
        return (ecx7 & (1u << 8)) != 0;
    return 0;
}
#endif /* OBLAS_ARCH_X86 */
#endif /* defined(_MSC_VER) && !defined(__clang__) */

#ifdef __cplusplus
extern "C" {
#endif

void obl_swap(uint8_t *a, uint8_t *b, unsigned k);
void *obl_alloc(size_t num_rows, size_t row_size, size_t alignment);
void obl_free(void *ptr);

#ifdef __cplusplus
}
#endif

#endif /* OBLAS_COMMON_H */
