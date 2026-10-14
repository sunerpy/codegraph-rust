#import "App-Swift.h"

@implementation Caller
- (void)run {
    Greeter *greeter = [[Greeter alloc] init];
    [greeter greet:@"world"];
}
@end
