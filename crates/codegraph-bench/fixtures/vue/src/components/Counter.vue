<template>
  <button @click="bump">{{ total }}</button>
</template>

<script lang="ts">
import { helper } from '../composables/useCounter';

export default {
  props: { start: Number },
  data() {
    return { total: 0 };
  },
  computed: {
    doubled() {
      return this.total * 2;
    },
  },
  methods: {
    bump() {
      this.total += helper();
    },
  },
  mounted() {
    this.bump();
  },
};
</script>
