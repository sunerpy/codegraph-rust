import { requireNativeModule } from 'expo-modules-core';

const HelloModule = requireNativeModule('Hello');

export function hello(name: string): string {
  return HelloModule.hello(name);
}
