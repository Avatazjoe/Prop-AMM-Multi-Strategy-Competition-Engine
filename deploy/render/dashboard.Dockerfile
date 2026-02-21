FROM node:20-alpine AS build
WORKDIR /app

COPY apps/dashboard/package.json /app/package.json
RUN npm install
COPY apps/dashboard /app

ARG VITE_API_BASE_URL
ENV VITE_API_BASE_URL=${VITE_API_BASE_URL}
RUN npm run build

FROM nginx:alpine
COPY --from=build /app/dist /usr/share/nginx/html
EXPOSE 10000
CMD ["nginx", "-g", "daemon off;"]
