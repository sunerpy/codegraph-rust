<template>
  <div class="sample">
    <MyComponent />
    <my-other-component />
    <Transition>
      <p>Hello</p>
    </Transition>
  </div>
</template>

<script lang="ts">
import { ref } from 'vue';
import MyComponent from './MyComponent.vue';

export function doSomething() {
  console.log('doing something');
}
</script>

<style scoped>
.sample {
  color: red;
}
</style>
