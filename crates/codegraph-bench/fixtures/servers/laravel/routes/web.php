<?php

use App\Http\Controllers\UserController;
use Illuminate\Support\Facades\Route;

Route::get('/users', [UserController::class, 'index']);
Route::prefix('admin')->group(function () {
    Route::get('/stats', 'UserController@stats');
});
