<template>
  <div @click="go">{{ msg }} {{ count }}</div>
  <Counter :start="1" />
</template>

<script setup lang="ts">
import { helper } from '../composables/useCounter';
import { useCounter } from '../composables/useCounter';
import Counter from './Counter.vue';

const msg = helper();
const { count, increment } = useCounter(0);

function go() {
  increment();
  return helper();
}
</script>
