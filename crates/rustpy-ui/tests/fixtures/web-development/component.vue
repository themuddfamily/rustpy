<script setup lang="ts">
import { ref } from 'vue';
const count = ref<number>(1);
</script>
<template><button class="counter" @click="count++">{{ count }}</button></template>
<style scoped>.counter { color: rebeccapurple; }</style>
