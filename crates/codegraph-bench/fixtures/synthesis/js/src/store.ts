import { createAsyncThunk, createSlice } from '@reduxjs/toolkit';
import { defineStore } from 'pinia';

export const fetchUser = createAsyncThunk('user/fetch', async (id: string) => {
  return loadUser(id);
});

async function loadUser(id: string) {
  return { id };
}

export const userSlice = createSlice({
  name: 'user',
  initialState: { id: '' },
  reducers: {
    reset(state) {
      state.id = '';
    },
  },
});

export const useCartStore = defineStore('cart', {
  state: () => ({ items: [] as string[] }),
  actions: {
    add(item: string) {
      this.items.push(item);
    },
  },
});
