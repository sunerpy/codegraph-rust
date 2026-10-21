<template><p>{{ id }}</p></template>
<script setup lang="ts">
const route = useRoute();
const id = route.params.id;
</script>
