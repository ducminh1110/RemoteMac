#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
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

uint32_t rm_virtual_display_create(uint32_t width, uint32_t height, int hidpi, char *err, int errlen) {
    @autoreleasepool {
        Class D = NSClassFromString(@"CGVirtualDisplayDescriptor"), M = NSClassFromString(@"CGVirtualDisplayMode"),
              S = NSClassFromString(@"CGVirtualDisplaySettings"), V = NSClassFromString(@"CGVirtualDisplay");
        if (!D || !M || !S || !V) { fail(err, errlen, @"CGVirtualDisplay API not present on this macOS"); return 0; }
        if (width < 320 || height < 240 || width > 8192 || height > 8192) { fail(err, errlen, @"unsupported size"); return 0; }
        current = nil; // one virtual display per agent: replace the old one
        uint32_t scale = hidpi ? 2 : 1;
        CGVirtualDisplayDescriptor *d = [[D alloc] init];
        d.queue = dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE, 0);
        d.name = @"MacBridge Display";
        d.maxPixelsWide = width * scale;
        d.maxPixelsHigh = height * scale;
        // ~110 ppi at 1x, so macOS picks sensible UI sizes
        d.sizeInMillimeters = CGSizeMake(width * 25.4 / 110.0, height * 25.4 / 110.0);
        d.productID = 0x524D; d.vendorID = 0x3610; d.serialNum = 0x0001;
        CGVirtualDisplay *v = [[V alloc] initWithDescriptor:d];
        if (!v || v.displayID == 0) { fail(err, errlen, @"virtual display could not be created"); return 0; }
        CGVirtualDisplaySettings *s = [[S alloc] init];
        s.hiDPI = hidpi ? 1 : 0;
        // HiDPI: the full-pixel mode too, which macOS offers as width x height at 2x (some
        // systems, VMs among them, only list the Retina mode when it is there)
        s.modes = hidpi ? @[[[M alloc] initWithWidth:width height:height refreshRate:60], [[M alloc] initWithWidth:width * 2 height:height * 2 refreshRate:60]]
                        : @[[[M alloc] initWithWidth:width height:height refreshRate:60]];
        if (![v applySettings:s]) { fail(err, errlen, @"display mode was refused"); return 0; }
        current = v;
        return v.displayID;
    }
}

void rm_virtual_display_destroy(void) { current = nil; }
