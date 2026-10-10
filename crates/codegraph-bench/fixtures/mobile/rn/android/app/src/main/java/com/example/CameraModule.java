package com.example;

import com.facebook.react.bridge.Promise;
import com.facebook.react.bridge.ReactContextBaseJavaModule;
import com.facebook.react.bridge.ReactMethod;

public class CameraModule extends ReactContextBaseJavaModule {
    @Override
    public String getName() {
        return "CameraModule";
    }

    @ReactMethod
    public void takePicture(Promise promise) {
        promise.resolve(capture());
    }

    private String capture() {
        return "file://photo.jpg";
    }
}
