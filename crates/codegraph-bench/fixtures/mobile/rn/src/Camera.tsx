import { NativeEventEmitter, NativeModules, requireNativeComponent } from 'react-native';

const { CameraModule } = NativeModules;
export const PICTURE_TAKEN = 'onPictureTaken';
const emitter = new NativeEventEmitter(CameraModule);

export const CameraView = requireNativeComponent('RCTCameraView');

export function takePicture(): Promise<string> {
  return CameraModule.takePicture();
}

export function listen(callback: (uri: string) => void) {
  return emitter.addListener(PICTURE_TAKEN, callback);
}
