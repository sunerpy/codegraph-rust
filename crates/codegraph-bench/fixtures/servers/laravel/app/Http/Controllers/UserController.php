<?php

namespace App\Http\Controllers;

class UserController extends Controller
{
    public function index()
    {
        return view('users.index', ['users' => $this->load()]);
    }

    public function stats()
    {
        return redirect('/users');
    }

    private function load(): array
    {
        return [];
    }
}
