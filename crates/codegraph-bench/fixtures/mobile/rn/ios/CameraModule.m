#import <React/RCTBridgeModule.h>
#import <React/RCTEventEmitter.h>

@interface CameraModule : RCTEventEmitter <RCTBridgeModule>
@end

@implementation CameraModule

RCT_EXPORT_MODULE();

RCT_EXPORT_METHOD(takePicture:(RCTPromiseResolveBlock)resolve rejecter:(RCTPromiseRejectBlock)reject)
{
  resolve([self capture]);
  [self sendEventWithName:@"onPictureTaken" body:@{}];
}

- (NSString *)capture
{
  return @"file://photo.jpg";
}

@end
