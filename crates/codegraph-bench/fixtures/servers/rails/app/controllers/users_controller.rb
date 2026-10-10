class UsersController < ApplicationController
  def index
    render json: load_users
  end

  def show
    render json: { id: params[:id] }
  end

  private

  def load_users
    []
  end
end
