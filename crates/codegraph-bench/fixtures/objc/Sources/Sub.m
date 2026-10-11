#import "Base.h"

static void cancel(void) { }

@interface Sub : Base <Doer>
- (void)work;
@end

@implementation Sub
- (void)work {
    [self ping];
    [super ping];
    [Base new];
    cancel();
}
@end
