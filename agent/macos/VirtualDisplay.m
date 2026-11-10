#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#include <math.h>
#include "VirtualDisplay.h"

@interface CGVirtualDisplayDescriptor : NSObject
@property (retain, nonatomic) dispatch_queue_t queue;
@property (retain, nonatomic) NSString *name;
@property (nonatomic) unsigned int maxPixelsWide;
@property (nonatomic) unsigned int maxPixelsHigh;
@property (nonatomic) CGSize sizeInMillimeters;
@property (nonatomic) unsigned int productID;
@property (nonatomic) unsigned int vendorID;
@property (nonatomic) unsigned int serialNum;
@end

@interface CGVirtualDisplayMode : NSObject
- (instancetype)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)refreshRate;
@end

@interface CGVirtualDisplaySettings : NSObject
@property (nonatomic) unsigned int hiDPI;
@property (retain, nonatomic) NSArray *modes;
@end

@interface CGVirtualDisplay : NSObject
- (instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@property (readonly, nonatomic) unsigned int displayID;
@end

static CGVirtualDisplay *current;

static void fail(char *err, int len, NSString *why) {
    if (err && len > 0) { strlcpy(err, why.UTF8String, (size_t)len); }
}

static NSArray *modes_at(Class M, uint32_t width, uint32_t height, int hidpi, double hz) {
    NSMutableArray *a = [NSMutableArray arrayWithObject:[[M alloc] initWithWidth:width height:height refreshRate:hz]];
    if (hidpi) { [a addObject:[[M alloc] initWithWidth:width * 2 height:height * 2 refreshRate:hz]]; }
    return a;
}

uint32_t rm_virtual_display_create(uint32_t width, uint32_t height, int hidpi, double refresh, char *err, int errlen) {
    @autoreleasepool {
        Class D = NSClassFromString(@"CGVirtualDisplayDescriptor"), M = NSClassFromString(@"CGVirtualDisplayMode"),
              S = NSClassFromString(@"CGVirtualDisplaySettings"), V = NSClassFromString(@"CGVirtualDisplay");
        if (!D || !M || !S || !V) { fail(err, errlen, @"CGVirtualDisplay API not present on this macOS"); return 0; }
        if (width < 320 || height < 240 || width > 8192 || height > 8192) { fail(err, errlen, @"unsupported size"); return 0; }
        if (!(refresh >= 1 && refresh <= 240)) { refresh = 60; }
        current = nil; // one virtual display per agent: replace the old one
        uint32_t scale = hidpi ? 2 : 1;
        CGVirtualDisplayDescriptor *d = [[D alloc] init];
        d.queue = dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE, 0);
        d.name = @"Taro Display";
        d.maxPixelsWide = width * scale;
        d.maxPixelsHigh = height * scale;
        // ~110 ppi at 1x, so macOS picks sensible UI sizes
        d.sizeInMillimeters = CGSizeMake(width * 25.4 / 110.0, height * 25.4 / 110.0);
        d.productID = 0x524D; d.vendorID = 0x3610; d.serialNum = 0x0001;
        CGVirtualDisplay *v = [[V alloc] initWithDescriptor:d];
        if (!v || v.displayID == 0) { fail(err, errlen, @"virtual display could not be created"); return 0; }
        CGVirtualDisplaySettings *s = [[S alloc] init];
        s.hiDPI = hidpi ? 1 : 0;
        // 60 first so the display comes up on a rate every Mac takes, then the faster modes.
        // HiDPI: the full-pixel mode too, which macOS offers as width x height at 2x (some
        // systems, VMs among them, only list the Retina mode when it is there)
        double rates[] = {60, 120, refresh};
        NSMutableArray *modes = [NSMutableArray array];
        for (int i = 0; i < 3; i++) {
            BOOL seen = NO;
            for (int j = 0; j < i; j++) { if (fabs(rates[j] - rates[i]) < 0.01) { seen = YES; break; } }
            if (!seen) { [modes addObjectsFromArray:modes_at(M, width, height, hidpi, rates[i])]; }
        }
        s.modes = modes;
        BOOL ok = [v applySettings:s];
        if (!ok && refresh > 120) {
            // a rate this Mac refuses: try 120, then 60, rather than lose the display
            s.modes = modes_at(M, width, height, hidpi, 120);
            ok = [v applySettings:s];
        }
        if (!ok && refresh != 60) {
            s.modes = modes_at(M, width, height, hidpi, 60);
            ok = [v applySettings:s];
        }
        if (!ok) { fail(err, errlen, @"display mode was refused"); return 0; }
        current = v;
        return v.displayID;
    }
}

void rm_virtual_display_destroy(void) { current = nil; }
