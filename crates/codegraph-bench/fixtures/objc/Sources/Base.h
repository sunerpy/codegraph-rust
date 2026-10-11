#import <Foundation/Foundation.h>

@protocol Doer
- (void)work;
@end

@interface Base : NSObject
- (void)ping;
@end
