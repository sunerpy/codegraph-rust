import { createAsyncThunk } from '@reduxjs/toolkit';

export const refreshCart = createAsyncThunk('cart/refresh', async (id: string) => {
  return { id };
});

export const checkout = createAsyncThunk('cart/checkout', async (id: string, { dispatch }) => {
  await dispatch(refreshCart(id));
});
